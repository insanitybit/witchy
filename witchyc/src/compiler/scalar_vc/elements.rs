//! Invariants over a container's elements (docs/proposal-element-invariants.md).
//!
//! `forall i in (0, v.len()): P(v[i], own fields)` of a sealed type is assumed of every value
//! of the type a function of its file receives, and kept at every change, so it can be used at
//! every read. No quantifier reaches the prover: each obligation is about one arbitrary element
//! or one arbitrary index, a fresh name.
//!
//! Two walks meet here. A function that receives such a value (`Watched`) shows the invariant
//! after each call that changes the elements, from what the callee's contract says it does to
//! them. A function that says what it does to a buffer's cells (`elements.` clauses; `Frame`)
//! shows that from what its own body does, in order.
use super::*;
use super::claims::{Claims, Effect, Held};

/// One element invariant of one parameter, as the walk keeps it.
#[derive(Clone)]
pub(super) struct Watched {
    /// The parameter, and the fields from it to the buffer the elements are cells of.
    root: u32,
    buffer: Vec<u32>,
    /// The fields from the parameter to the number of elements.
    length: Vec<u32>,
    body: Prop,
    /// The value's own number fields the clause reads.
    own: Vec<(Vec<u32>, Ty)>,
    span: std::ops::Range<usize>,
}
/// What an element's fields are read from when the clause is stated of it.
enum Element {
    /// A local (or part of one) that holds the element, or refers to it.
    Place(Key),
    /// Any element at all: a name for each field, made when first read.
    Any(BTreeMap<Vec<u32>, Math>),
}
/// Where a value stored in a cell came from.
#[derive(Clone, PartialEq)]
enum Origin {
    /// A parameter taken by value, never written since entry.
    Parameter(u32),
    /// The cell at this index as the function was entered.
    Old(Math),
    Unknown,
}
/// One thing done to a buffer's cells since the function was entered.
#[derive(Clone, PartialEq)]
enum Event {
    /// Cells at or past this index may have changed.
    Below(Math),
    /// The cell at this index may have changed, now or at any later point.
    Except(Math),
    /// The cell at this index was given this value; no other cell changed.
    Write(Math, Origin),
}
/// What the body has done so far to one buffer its contract speaks of.
#[derive(Clone, PartialEq, Default)]
pub(super) struct Frame {
    /// Something was done that the walk does not follow: nothing can be shown.
    poisoned: bool,
    /// In order. An event with a condition was done only on a way where the condition held
    /// (the ways joined after a branch): its index means nothing on the others.
    log: Vec<(Option<Prop>, Event)>,
}
/// What is known along the way being walked, kept and joined as `state` is.
#[derive(Clone, Default)]
pub(super) struct Tags {
    /// Slice locals made from a watched container's counted view.
    slices: BTreeMap<u32, usize>,
    /// Locals holding the value a cell had on entry: the buffer, and the cell's index.
    olds: BTreeMap<u32, (usize, Math)>,
    /// `&mut` locals that refer to one cell, used for nothing but one store through them.
    cells: BTreeMap<u32, (usize, Math)>,
    frames: Vec<Frame>,
    /// Changes whenever `frames` does.
    version: u64,
}
/// What a call may do to a watched value, noted before its arguments are forgotten.
pub(super) struct Lent {
    watched: usize,
    /// The argument that reaches the buffer, and the fields from the callee's parameter to it;
    /// `None` when the call cannot reach the buffer.
    buffer: Option<(usize, Vec<u32>)>,
    /// A `&mut` argument reaches the buffer another way: nothing is known of what it does.
    lost: bool,
    /// The whole value is lent: its own file's functions keep its invariants.
    whole: bool,
    length: Math,
    own: Vec<Math>,
}
/// What a call's result says of elements, until the result is bound or matched.
#[derive(Clone, Default)]
pub(super) struct Elements {
    /// The result (for a `Result`, this variant's value) is an element: which invariant, and
    /// what shows its index is one (`index < length`).
    /// The clause is of the value's own fields as they were when the call read the element:
    /// by the time the result is matched they may be another value's (an exchange of the whole).
    reads: Vec<(Option<u8>, usize, Prop, Vec<Math>)>,
    /// The result holds what this cell held on entry.
    olds: Vec<(Option<u8>, usize, Math)>,
    /// The result refers to this cell.
    cells: Vec<(Option<u8>, usize, Math)>,
    /// What the call did to each buffer, by variant, where the variants differ: while
    /// `Tags::version` is still this, a variant's own can take the place of all of them joined.
    strong: Option<(u64, Vec<Vec<Frame>>)>,
}

/// The buffer a view method lends as a slice, and the field it reads the count from: its body
/// is `return memory.slice_unchecked(&(*value).B, 0, (*value).L)`. Read from the source as
/// written, never from a name.
fn view_storage(functions: &[Function], view: u32) -> Option<(Vec<u32>, Vec<u32>)> {
    let view = functions.get(view as usize)?;
    if view.params.len() != 1 { return None; }
    let [Stmt::Return(e, _, _)] = view.body.as_slice() else { return None };
    let Value::Storage { op: StorageOp::SliceUnchecked, receiver, index, value: Some(to), .. } = &e.kind else { return None };
    if !matches!(index.kind, Value::U64(0)) { return None; }
    let buffer = box_place(receiver).filter(|place| place.root == 0)?;
    let Value::ReadPlace(length) = &to.kind else { return None };
    if length.root != 0 { return None; }
    Some((fields(buffer)?, fields(length)?))
}
/// The same of a container's `index` method: its body is `return c.m()[i]`, `m` such a view.
pub(super) fn storage(functions: &[Function], index: u32) -> Option<(Vec<u32>, Vec<u32>)> {
    let reader = functions.get(index as usize)?;
    if reader.params.len() != 2 { return None; }
    let [Stmt::Return(e, _, _)] = reader.body.as_slice() else { return None };
    let Value::SliceIndex(slice, at, _) = &e.kind else { return None };
    if !matches!(&at.kind, Value::ReadPlace(place) if place.root == 1 && place.projections.is_empty()) { return None; }
    let Value::Call(call) = &slice.kind else { return None };
    let [receiver] = call.args.as_slice() else { return None };
    if !box_place(receiver).is_some_and(|place| place.root == 0 && place.projections.is_empty()) { return None; }
    view_storage(functions, call.definition.0)
}
/// A method that lends one cell for changing it: its body is
/// `if c: return Ok(memory.mut_unchecked(&mut (*value).B, index))` and `return Err(k)`, the
/// test and the error without a call. The buffer, and which parameter is the index. When it
/// answers `Ok`, it has changed nothing, and the reference is to that cell and no other.
fn cell_lender(functions: &[Function], callee: u32) -> Option<(Vec<u32>, u32)> {
    fn plain(e: &Expr) -> bool {
        match &e.kind {
            Value::U64(_) | Value::Bool(_) => true,
            Value::ReadPlace(place) => fields(place).is_some(),
            Value::Binary(_, a, b) => plain(a) && plain(b),
            Value::Not(inner) | Value::Widen(inner) => plain(inner),
            _ => false,
        }
    }
    let callee = functions.get(callee as usize)?;
    let [Stmt::If(test, yes, no), Stmt::Return(other, _, _)] = callee.body.as_slice() else { return None };
    let ([Stmt::Return(lent, _, _)], []) = (yes.as_slice(), no.as_slice()) else { return None };
    if !plain(test) || !matches!(&other.kind, Value::Sum { arm, payload } if *arm != 0 && plain(payload)) { return None; }
    let Value::Sum { arm: 0, payload } = &lent.kind else { return None };
    let Value::Storage { op: StorageOp::MutUnchecked, receiver, index, value: None, .. } = &payload.kind else { return None };
    let buffer = box_place(receiver).filter(|place| place.root == 0)?;
    let Value::ReadPlace(index) = &index.kind else { return None };
    if !index.projections.is_empty() || index.pointer || index.root == 0 || index.root as usize >= callee.params.len() { return None; }
    Some((fields(buffer)?, index.root))
}
/// A function that never returns: its body is one `unreachable`.
pub(super) fn diverges(functions: &[Function], e: &Expr) -> bool {
    matches!(&e.kind, Value::Call(call) if matches!(functions[call.definition.0 as usize].body.as_slice(), [Stmt::Trap(_)]))
}
fn overlaps(a: &[u32], b: &[u32]) -> bool { a.starts_with(b) || b.starts_with(a) }
/// Whether the binding of a match arm is used for one thing only: the store through it that
/// the arm begins with. Counted on the arm as it is printed, where every use of a local is a
/// place with that root: anything not recognised is another use.
fn only_stored(binding: u32, body: &[Stmt]) -> bool {
    let Some(Stmt::StorePlace(place, None, _)) = body.first() else { return false };
    if place.root != binding || !place.pointer || !place.projections.is_empty() { return false; }
    let text = format!("{body:?}");
    text.matches(&format!("root: {binding},")).count() == 1 && !text.contains(&format!("Set({binding},")) && !text.contains(&format!("Replace({binding},"))
}
fn conjoin(given: &Option<Prop>, goal: Prop) -> Prop { match given { Some(given) => implies(given, goal), None => goal } }
const NUMBER: Ty = Ty::Unsigned(crate::compiler::scalar::Scalar::U64);

impl Frame {
    /// The two ways here, `mine` and `theirs` what held on each.
    fn join(&mut self, other: &Frame, mine: &Option<Prop>, theirs: &Option<Prop>) {
        self.poisoned |= other.poisoned;
        if self.log == other.log { return; }
        // Either way was taken: what either did after what both did may have been done, and
        // no cell's value is known from a store only one of them made.
        let common = self.log.iter().zip(&other.log).take_while(|(a, b)| a == b).count();
        let under = |way: &Option<Prop>, (condition, event): &(Option<Prop>, Event)| {
            let condition = match (way, condition) { (Some(a), Some(b)) => Some(all(vec![a.clone(), b.clone()])), (Some(a), None) => Some(a.clone()), (None, b) => b.clone() };
            (condition, match event { Event::Write(at, _) => Event::Except(at.clone()), other => other.clone() })
        };
        let rest: Vec<(Option<Prop>, Event)> = self.log[common..].iter().map(|event| under(mine, event)).chain(other.log[common..].iter().map(|event| under(theirs, event))).collect();
        self.log.truncate(common);
        self.log.extend(rest);
    }
}
impl Tags {
    pub(super) fn join(&mut self, other: &Tags, stamp: u64, mine: &Option<Prop>, theirs: &Option<Prop>) {
        self.slices.retain(|local, at| other.slices.get(local) == Some(at));
        self.olds.retain(|local, at| other.olds.get(local) == Some(at));
        self.cells.retain(|local, at| other.cells.get(local) == Some(at));
        for (frame, other) in self.frames.iter_mut().zip(&other.frames) { frame.join(other, mine, theirs); }
        self.version = stamp;
    }
}

impl Context<'_> {
    pub(super) fn stamp(&mut self) -> u64 { self.stamps += 1; self.stamps }
    /// The element invariants of what this function receives, and the buffers its contract
    /// says what it does to.
    pub(super) fn watch(&mut self, contract: &super::super::contract::Contract) -> Result<()> {
        for invariant in &contract.elements {
            let refused = |what: &str| Diagnostic::new("W0401", invariant.span.clone(), format!("a type invariant over elements {what}"));
            let Some((root, length, _, false)) = read(self.functions, &invariant.upper)? else { return Err(refused("ends its range at the length of a field")) };
            if root as usize >= self.function.params.len() { return Err(refused("is of a value the function receives")); }
            let (mut buffer, mut own, mut parts, mut reader) = (None, Vec::new(), Vec::new(), None);
            invariant.body.observes(&mut parts);
            for part in &parts {
                if let Math::Field { path, .. } = part && let Some(step) = path.iter().find(|step| (ELEMENT_METHOD..ELEMENT_METHOD + VIEW_METHOD).contains(*step)) { reader = Some(*step - ELEMENT_METHOD); }
                let Some((of, path, ty, before)) = read(self.functions, part)? else { continue };
                if of != root || before { return Err(refused("reads only the value's own fields and its elements")); }
                match path.iter().position(|step| *step == ANY_ELEMENT) {
                    Some(at) => {
                        if at + 1 == path.len() || path[at + 1..].contains(&ANY_ELEMENT) { return Err(refused("reads a number field of each element (`field[i].part`)")); }
                        if buffer.as_deref().is_some_and(|known: &[u32]| known != &path[..at]) { return Err(refused("reads the elements of one field")); }
                        buffer = Some(path[..at].to_vec());
                    }
                    None => if !own.contains(&(path.clone(), ty)) { own.push((path, ty)); },
                }
            }
            let (Some(buffer), Some(reader)) = (buffer, reader) else { return Err(refused("reads `field[i]`")) };
            // The range ends at the count the elements are read below: the field the same
            // container's view reads its length from.
            let Some((cells, count)) = storage(self.functions, reader) else { return Err(refused("reads elements through an `index` method of the form `return c.view()[i]`")) };
            let Some(container) = buffer.strip_suffix(cells.as_slice()) else { return Err(refused("reads elements through an `index` method of the form `return c.view()[i]`")) };
            if length != join(container, &count) { return Err(refused("ends its range at the length of the field whose elements it reads")); }
            self.watched.push(Watched { root, buffer, length, body: invariant.body.clone(), own, span: invariant.span.clone() });
        }
        for stated in &contract.effects {
            let Some((root, buffer, _, false)) = read(self.functions, &stated.buffer)? else { return Err(Diagnostic::new("W0401", stated.span.clone(), "an `elements.` clause names a buffer: a field of a parameter")) };
            if !matches!(self.function.params.get(root as usize), Some(Ty::Borrow { mutable: true, .. })) { return Err(Diagnostic::new("W0401", stated.span.clone(), "an `elements.` clause names a buffer of a `&mut` parameter")); }
            if !self.declared.contains(&(root, buffer.clone())) { self.declared.push((root, buffer)); self.tags.frames.push(Frame::default()); }
        }
        Ok(())
    }
    /// The clause of one element, with the value's own fields as they are now, or as `own` has them.
    fn instance(&mut self, watched: &Watched, element: &mut Element, own: Option<&[Math]>) -> Result<Prop> {
        let functions = self.functions;
        watched.body.clone().rewrite(&mut |m| {
            let Some((root, path, ty, _)) = read(functions, m)? else { return Ok(m.clone()) };
            Ok(match path.iter().position(|step| *step == ANY_ELEMENT) {
                Some(at) => {
                    let rest = path[at + 1..].to_vec();
                    match element {
                        Element::Place((local, base)) => self.leaf((*local, join(base, &rest)), ty),
                        Element::Any(names) => match names.get(&rest) { Some(name) => name.clone(), None => { let name = self.fresh(ty); names.insert(rest, name.clone()); name } },
                    }
                }
                None => match (own, watched.own.iter().position(|(known, _)| *known == path)) {
                    (Some(values), Some(at)) => values[at].clone(),
                    _ => self.leaf((root, path), ty),
                },
            })
        })
    }
    fn own_now(&mut self, watched: &Watched) -> Vec<Math> {
        watched.own.clone().into_iter().map(|(path, ty)| self.leaf((watched.root, path), ty)).collect()
    }
    fn refuse(&mut self, watched: usize, what: &str) {
        if self.element_refusal.is_some() { return; }
        let span = self.watched[watched].span.clone();
        self.element_refusal = Some(Diagnostic::new("W0401", span, format!("{what}, in a way this invariant over elements does not follow: change the elements through calls whose contracts say what they do to them (`push_mut`, `v[i] = x`, `take_last_mut`), and build the value with the field empty")));
    }
    fn poison(&mut self, frame: usize) {
        self.tags.frames[frame].poisoned = true;
        self.tags.version = self.stamp();
    }
    fn record(&mut self, frame: usize, event: Event) {
        self.tags.frames[frame].log.push((None, event));
        self.tags.version = self.stamp();
    }
    /// Something the walk no longer follows under `path` of `root`.
    pub(super) fn element_forgotten(&mut self, root: u32, path: &[u32]) {
        if path.is_empty() { self.tags.slices.remove(&root); self.tags.olds.remove(&root); self.tags.cells.remove(&root); }
        if (root as usize) < self.function.params.len() { self.dirty.insert(root); }
        if self.element_guard { return; }
        // An invariant that reads it can no longer be kept.
        for at in 0..self.watched.len() {
            let watched = &self.watched[at];
            if watched.root != root { continue; }
            if overlaps(&watched.buffer, path) || overlaps(&watched.length, path) || watched.own.iter().any(|(own, _)| overlaps(own, path)) {
                self.refuse(at, "the elements, or a field the invariant reads, may change here");
            }
        }
        // And nothing more can be said of what was done to its cells.
        if self.excused { return; }
        for at in 0..self.declared.len() {
            if self.declared[at].0 == root && overlaps(&self.declared[at].1, path) { self.poison(at); }
        }
    }
    /// What each invariant reads of the value's own fields, before `key` is assigned, when one reads it.
    pub(super) fn own_before(&mut self, key: &Key) -> Option<Vec<Vec<Math>>> {
        if (key.0 as usize) < self.function.params.len() { self.dirty.insert(key.0); }
        if !self.watched.iter().any(|watched| watched.root == key.0 && (watched.own.iter().any(|(own, _)| *own == key.1) || watched.length == key.1)) { return None; }
        Some((0..self.watched.len()).map(|at| { let watched = self.watched[at].clone(); self.own_now(&watched) }).collect())
    }
    /// One of the value's own fields was assigned: the clause, true of an arbitrary element
    /// with the field as it was, is shown of it with the field as it is.
    pub(super) fn own_assigned(&mut self, key: &Key, before: Option<Vec<Vec<Math>>>) -> Result<()> {
        let Some(before) = before else { return Ok(()) };
        for at in 0..self.watched.len() {
            let watched = self.watched[at].clone();
            if watched.root != key.0 { continue; }
            if watched.length == key.1 { self.refuse(at, "the count of elements is assigned here"); continue; }
            if !watched.own.iter().any(|(own, _)| *own == key.1) { continue; }
            self.step(&watched, &before[at], &None)?;
        }
        Ok(())
    }
    /// `P(e, before) -> P(e, now)` of an arbitrary element `e`, under `given`.
    fn step(&mut self, watched: &Watched, before: &[Math], given: &Option<Prop>) -> Result<()> {
        let now = self.own_now(watched);
        if now.as_slice() == before { return Ok(()); }
        let mut any = Element::Any(BTreeMap::new());
        let held = self.instance(watched, &mut any, Some(before))?;
        let holds = self.instance(watched, &mut any, Some(&now))?;
        self.shown("an invariant over elements, of every element, after a field it reads changes", conjoin(given, implies(&held, holds)), &watched.span)
    }
    fn shown(&mut self, what: &'static str, goal: Prop, span: &std::ops::Range<usize>) -> Result<()> {
        self.body(what, goal)?;
        if let Some(last) = self.obligations.last_mut() { last.site = Some((span.start, span.end)); }
        Ok(())
    }
    /// Before a call's `&mut` arguments are forgotten: which watched values it may change, and
    /// what is known of them now.
    pub(super) fn elements_before(&mut self, arguments: &[Argument]) -> Vec<Lent> {
        let mut lent = Vec::new();
        for at in 0..self.watched.len() {
            let watched = self.watched[at].clone();
            let mut found: Option<Lent> = None;
            for (index, argument) in arguments.iter().enumerate() {
                let Argument::Place { root, path, changed: true } = argument else { continue };
                if *root != watched.root { continue; }
                let reaches = overlaps(&watched.buffer, path);
                if !(reaches || overlaps(&watched.length, path) || watched.own.iter().any(|(own, _)| overlaps(own, path))) { continue; }
                if found.is_none() {
                    let length = self.leaf((watched.root, watched.length.clone()), NUMBER);
                    let own = self.own_now(&watched);
                    found = Some(Lent { watched: at, buffer: None, lost: false, whole: false, length, own });
                }
                let known = found.as_mut().expect("just made");
                known.whole |= path.is_empty();
                if reaches {
                    if known.buffer.is_some() || !watched.buffer.starts_with(path) { known.lost = true; }
                    else { known.buffer = Some((index, watched.buffer[path.len()..].to_vec())); }
                }
            }
            lent.extend(found);
        }
        lent
    }
    /// After a call. Each watched value it may have changed still satisfies its invariant: per
    /// variant of the result, under that variant's facts, every index below the count is a
    /// kept index below the old count or one of the cells written, the clause holds of each
    /// value written, and of an arbitrary element if the value's own fields changed. And what
    /// the call did to each buffer this function's contract speaks of is noted.
    pub(super) fn elements_after(&mut self, call: &InstanceCall, lent: Vec<Lent>, claims: &Claims, resolved: &[Option<Math>], arguments: &[Argument], outcome: &mut Outcome) -> Result<()> {
        let arms: Vec<Option<u8>> = match self.types.sum(call.result) { Some(sum) => (0..sum.arms.len() as u8).map(Some).collect(), None => vec![None] };
        let site = call.contract_site.and_then(|site| self.function.call_spans.get(site as usize)).copied();
        let name = &self.functions[call.definition.0 as usize].name;
        let lender = cell_lender(self.functions, call.definition.0);
        // What evaluating the arguments may change: a call inside a later argument runs after an
        // earlier one was copied (`put(&mut v, item, fix(&mut item))`).
        let assigned = scan::assigned_by(&call.args);
        let said = |argument: usize, buffer: &[u32], arm: Option<u8>| -> Vec<(Effect, Option<Math>)> {
            claims.elements.iter().enumerate().filter(|(_, claim)| claim.parameter as usize == argument && claim.buffer == buffer && (claim.arm.is_none() || claim.arm == arm))
                .map(|(at, claim)| (claim.effect, resolved[at].clone())).collect()
        };
        // The count of each watched container before the call, for a cell it took a value out of.
        let counts: Vec<(usize, Math)> = lent.iter().filter(|known| !known.whole && !known.lost).map(|known| (known.watched, known.length.clone())).collect();
        for known in lent {
            let watched = self.watched[known.watched].clone();
            // The value lent whole is changed only by the functions of its own file, each of
            // which keeps its invariants: they hold of it as it is now.
            if known.whole { continue; }
            let unsaid = || {
                let mut refusal = Diagnostic::new("W0401", watched.span.clone(), format!("`{name}` is given the elements this invariant speaks of, and its contract does not say what it does to them: nothing shows the invariant still holds"));
                if let Some((start, end)) = site { refusal.span = start..end; }
                refusal
            };
            if known.lost { return Err(unsaid()); }
            for arm in &arms {
                let facts: Vec<Prop> = outcome.facts.iter().filter(|(of, _)| Some(*of) == *arm).map(|(_, fact)| fact.clone()).collect();
                let given = (!facts.is_empty()).then(|| all(facts));
                self.step(&watched, &known.own, &given)?;
                let Some((argument, buffer)) = &known.buffer else { continue };
                let length = self.leaf((watched.root, watched.length.clone()), NUMBER);
                let any = self.fresh(NUMBER);
                // The ways an index below the count now is one whose element satisfies the clause.
                let mut ways = Vec::new();
                for (effect, index) in said(*argument, buffer, *arm) {
                    let was = Prop::Compare("<", any.clone(), known.length.clone());
                    let needed = |index: Option<Math>| index.ok_or_else(&unsaid);
                    match effect {
                        Effect::Kept => ways.push(was),
                        Effect::KeptBelow => ways.push(all(vec![was, Prop::Compare("<", any.clone(), needed(index)?)])),
                        Effect::KeptExcept => ways.push(all(vec![was, Prop::Compare("≠", any.clone(), needed(index)?)])),
                        Effect::Among => { let below = needed(index)?; ways.push(all(vec![Prop::Compare("<", any.clone(), below.clone()), Prop::Compare("≤", below, known.length.clone())])); }
                        Effect::Holds(Held::Parameter(parameter)) => {
                            // The value stored is the argument as it was when the call was made: a
                            // name the same call may change (`put(&mut v, i, item, &mut item)`),
                            // or a later argument's own call, says nothing of it afterwards.
                            let named = match arguments.get(parameter as usize) {
                                Some(Argument::Place { root, path, changed: false }) if !changes(arguments, *root) && !assigned.contains(root) => Some((*root, path.clone())),
                                _ => None,
                            };
                            let Some((root, path)) = named else {
                                let mut refusal = error("an invariant over elements is shown of the value this call stores, which is not a name the call leaves alone: bind the value with `let` first and pass that name");
                                if let Some((start, end)) = site { refusal.span = start..end; }
                                return Err(refusal);
                            };
                            let holds = self.instance(&watched, &mut Element::Place((root, path)), None)?;
                            self.shown("an invariant over elements, of an element stored here", conjoin(&given, holds), &watched.span)?;
                            ways.push(equal(any.clone(), needed(index)?));
                        }
                        Effect::Holds(Held::Result) | Effect::Was(_) => {}
                    }
                }
                if ways.is_empty() { return Err(unsaid()); }
                let mut covered = ways.remove(0);
                for way in ways { covered = Prop::Binary("∨", Box::new(covered), Box::new(way)); }
                self.shown("an invariant over elements: every element after this call is one kept or one stored by it", conjoin(&given, implies(&Prop::Compare("<", any, length), covered)), &watched.span)?;
            }
        }
        // An element read: `memory.cell(buffer, index, result)`, of a buffer not lent with `&mut`
        // (whose elements, and the fields the clause reads, are as they were before the call).
        for (at, claim) in claims.elements.iter().enumerate() {
            if !matches!(claim.effect, Effect::Holds(Held::Result) | Effect::Was(Held::Result)) { continue; }
            let (Some(Argument::Place { root, path, changed }), Some(index)) = (arguments.get(claim.parameter as usize), &resolved[at]) else { continue };
            let buffer = join(path, &claim.buffer);
            // Every invariant over this buffer's elements (a type may state several).
            let all: Vec<usize> = self.watched.iter().enumerate().filter(|(_, watched)| watched.root == *root && watched.buffer == buffer).map(|(at, _)| at).collect();
            for watched in all {
                if !*changed {
                    let length = self.watched[watched].length.clone();
                    let length = self.leaf((*root, length), NUMBER);
                    let own = { let of = self.watched[watched].clone(); self.own_now(&of) };
                    outcome.elements.reads.push((claim.arm, watched, Prop::Compare("<", index.clone(), length), own));
                } else if claim.effect == Effect::Was(Held::Result) && !self.watched[watched].own.iter().any(|(own, _)| overlaps(own, path))
                    && let Some((_, length)) = counts.iter().find(|(of, _)| *of == watched) {
                    // A value the call took out of a cell: an element before the call, whose
                    // clause read the value's own fields as they still are (the call was not
                    // lent them).
                    let own = { let of = self.watched[watched].clone(); self.own_now(&of) };
                    outcome.elements.reads.push((claim.arm, watched, Prop::Compare("<", index.clone(), length.clone()), own));
                }
            }
            // The value a cell had on entry, while nothing has been done to the buffer.
            if let Some(frame) = self.declared.iter().position(|(of, known)| of == root && *known == buffer)
                && !self.tags.frames[frame].poisoned && self.tags.frames[frame].log.is_empty() {
                outcome.elements.olds.push((claim.arm, frame, index.clone()));
            }
        }
        // What the call did to the buffers this function's contract speaks of.
        let mut strong: Vec<Vec<Frame>> = arms.iter().map(|_| self.tags.frames.clone()).collect();
        let mut touched = false;
        for frame in 0..self.declared.len() {
            let (root, buffer) = self.declared[frame].clone();
            let reaching: Vec<(usize, &Vec<u32>)> = arguments.iter().enumerate().filter_map(|(index, argument)| match argument {
                Argument::Place { root: of, path, changed: true } if *of == root && overlaps(&buffer, path) => Some((index, path)), _ => None }).collect();
            if reaching.is_empty() { continue; }
            touched = true;
            let [(argument, path)] = reaching.as_slice() else { for way in &mut strong { way[frame].poisoned = true; } continue };
            let Some(rest) = buffer.strip_prefix(path.as_slice()) else { for way in &mut strong { way[frame].poisoned = true; } continue };
            for (way, arm) in strong.iter_mut().zip(&arms) {
                // A cell lent to be written: nothing else is changed, and that cell may be at any time.
                if let (Some((cells, index)), Some(0), 0) = (&lender, arm, *argument) && cells.as_slice() == rest
                    && let Some(Argument::Scalar(index)) = arguments.get(*index as usize) {
                    way[frame].log.push((None, Event::Except(index.clone())));
                    outcome.elements.cells.push((*arm, frame, index.clone()));
                    continue;
                }
                let effects = said(*argument, rest, *arm);
                let (mut kept, mut lost) = (false, false);
                for (effect, index) in &effects {
                    match (effect, index) {
                        (Effect::Kept, _) => kept = true,
                        (Effect::KeptBelow, Some(index)) => { kept = true; way[frame].log.push((None, Event::Below(index.clone()))); }
                        (Effect::KeptExcept, Some(index)) => { kept = true; way[frame].log.push((None, Event::Except(index.clone()))); }
                        (Effect::Holds(Held::Result), _) if self.types.lends_unique(call.result) => lost = true,
                        (Effect::Holds(_) | Effect::Was(_), _) => {}
                        _ => lost = true,
                    }
                }
                for (effect, index) in effects {
                    let (Effect::Holds(Held::Parameter(parameter)), Some(index)) = (effect, index) else { continue };
                    let origin = match arguments.get(parameter as usize) {
                        Some(Argument::Place { root, path, changed: false }) if path.is_empty() && !changes(arguments, *root) && !assigned.contains(root) => self.origin(*root, frame),
                        _ => Origin::Unknown,
                    };
                    way[frame].log.push((None, Event::Write(index, origin)));
                }
                if !kept || lost { way[frame].poisoned = true; }
            }
        }
        if touched {
            // Joined: each variant's own under what the callee says holds when it answers it.
            let when = |arm: &Option<u8>| { let facts: Vec<Prop> = outcome.facts.iter().filter(|(of, _)| Some(*of) == *arm).map(|(_, fact)| fact.clone()).collect(); (!facts.is_empty()).then(|| all(facts)) };
            let mut weak = strong[0].clone();
            for (at, way) in strong.iter().enumerate().skip(1) {
                let (mine, theirs) = (if at == 1 { when(&arms[0]) } else { None }, when(&arms[at]));
                for (frame, other) in weak.iter_mut().zip(way) { frame.join(other, &mine, &theirs); }
            }
            self.tags.frames = weak;
            self.tags.version = self.stamp();
            if strong.len() > 1 { outcome.elements.strong = Some((self.tags.version, strong)); }
        }
        Ok(())
    }
    /// Whether the kept `&mut` this call answers is one cell of a buffer whose changes are
    /// followed: lending it is then no change the walk loses (`Event::Except`).
    pub(super) fn cell_lent(&self, call: &InstanceCall, arguments: &[Argument]) -> bool {
        let Some((cells, _)) = cell_lender(self.functions, call.definition.0) else { return false };
        matches!(arguments.first(), Some(Argument::Place { root, path, changed: true }) if self.declared.contains(&(*root, join(path, &cells))))
    }
    /// Where the value of a whole local came from, for a store of it into a cell of `frame`'s buffer.
    fn origin(&self, local: u32, frame: usize) -> Origin {
        if let Some((of, index)) = self.tags.olds.get(&local) && *of == frame { return Origin::Old(index.clone()); }
        if (local as usize) < self.function.params.len() && !matches!(self.function.params[local as usize], Ty::Borrow { .. }) && !self.dirty.contains(&local) { return Origin::Parameter(local); }
        Origin::Unknown
    }
    /// A variant of a call's result is taken: what the call did when it answered this one, and
    /// what its value is.
    pub(super) fn element_arm(&mut self, found: &Elements, arm: Option<u8>, binding: Option<u32>, body: &[Stmt]) -> Result<()> {
        if let (Some((version, strong)), Some(arm)) = (&found.strong, arm) && *version == self.tags.version && let Some(frames) = strong.get(arm as usize) {
            self.tags.frames = frames.clone();
            self.tags.version = self.stamp();
        }
        let Some(binding) = binding else { return Ok(()) };
        for (of, watched, within, own) in &found.reads {
            if of.is_some() && *of != arm { continue; }
            let watched = self.watched[*watched].clone();
            let holds = self.instance(&watched, &mut Element::Place((binding, Vec::new())), Some(own))?;
            self.hypotheses.push(implies(within, holds));
        }
        for (of, frame, index) in &found.olds { if of.is_none() || *of == arm { self.tags.olds.insert(binding, (*frame, index.clone())); } }
        for (of, frame, index) in &found.cells { if (of.is_none() || *of == arm) && only_stored(binding, body) { self.tags.cells.insert(binding, (*frame, index.clone())); } }
        Ok(())
    }
    /// The cell a store through this place is to, when the place is a reference to one. Asked
    /// before the store is walked, which forgets the reference.
    pub(super) fn element_cell(&mut self, place: &Place) -> Option<(usize, Math)> {
        if !place.pointer || !place.projections.is_empty() { return None; }
        self.tags.cells.remove(&place.root)
    }
    /// A store of a whole value through a reference to one cell: that cell now holds it.
    pub(super) fn element_stored(&mut self, cell: Option<(usize, Math)>, e: &Expr) {
        let Some((frame, index)) = cell else { return };
        let origin = match &e.kind {
            Value::ReadPlace(from) | Value::Move { place: from, .. } if from.projections.is_empty() && !from.pointer => self.origin(from.root, frame),
            _ => Origin::Unknown,
        };
        self.record(frame, Event::Write(index, origin));
    }
    /// A loop: what each pass does to a buffer is not followed.
    pub(super) fn element_loop(&mut self, assigned: &BTreeSet<u32>) {
        for frame in 0..self.declared.len() { self.poison(frame); }
        self.tags.slices.retain(|local, _| !assigned.contains(local));
        self.tags.olds.retain(|local, _| !assigned.contains(local));
        self.tags.cells.retain(|local, _| !assigned.contains(local));
    }
    /// At a return: each `elements.` clause of the contract, of the variant returned, from what
    /// the body did to the buffer on the way here.
    pub(super) fn effects_shown(&mut self, variant: &Returned) -> Result<()> {
        use super::super::contract::ElementKind;
        let function = self.function;
        let Some(contract) = scalar_contract(function) else { return Ok(()) };
        for stated in &contract.effects {
            let refused = |what: &str| Diagnostic::new("W0401", stated.span.clone(), format!("this `elements.` clause is not shown: {what}"));
            let mut given = None;
            let mut frames = self.tags.frames.clone();
            match (stated.arm, variant) {
                (Some(arm), Returned::Variant(returned)) if arm != *returned => continue,
                (Some(arm), Returned::Outcome(outcome)) => {
                    let facts: Vec<Prop> = outcome.facts.iter().filter(|(of, _)| *of == arm).map(|(_, fact)| fact.clone()).collect();
                    given = (!facts.is_empty()).then(|| all(facts));
                    if let Some((version, strong)) = &outcome.elements.strong && *version == self.tags.version && let Some(own) = strong.get(arm as usize) { frames = own.clone(); }
                }
                _ => {}
            }
            let Some((root, buffer, _, _)) = read(self.functions, &stated.buffer)? else { continue };
            let Some(at) = self.declared.iter().position(|known| *known == (root, buffer.clone())) else { continue };
            let frame = frames.swap_remove(at);
            if frame.poisoned { return Err(refused("the body changes the buffer in a way that is not followed (a loop, a store of the whole value, a kept `&mut`, or a call whose contract does not say what it does to the cells)")); }
            let term = |context: &mut Self, m: &Math| m.rewrite(&mut |m| match m {
                Math::Result => Err(error("an `elements.` clause does not read the result")),
                Math::Observe { .. } | Math::Field { .. } => context.observe(m),
                other => Ok(other.clone()),
            }).map_err(|refusal| super::super::proof_advice::at_clause(refusal, &stated.span));
            const WHAT: &str = "an `elements.` clause where the function returns";
            // Of any index the clause says is kept: nothing the body did reaches it. (A store
            // is to a cell some `Except` names, or changes no value: `Kept` with `Holds`.)
            let kept = |context: &mut Self, said: &dyn Fn(&Math) -> Prop| -> Result<()> {
                if frame.log.is_empty() { return Ok(()); }
                let any = context.fresh(NUMBER);
                let untouched = all(frame.log.iter().filter_map(|(when, event)| Some(conjoin(when, match event {
                    Event::Below(n) => Prop::Compare("<", any.clone(), n.clone()),
                    Event::Except(i) => Prop::Compare("≠", any.clone(), i.clone()),
                    Event::Write(..) => return None,
                }))).collect());
                context.shown(WHAT, conjoin(&given, implies(&said(&any), untouched)), &stated.span)
            };
            match &stated.kind {
                ElementKind::Kept => if !frame.log.is_empty() { return Err(refused("the body changes cells of the buffer")); },
                ElementKind::KeptBelow(n) => { let n = term(self, n)?; kept(self, &|any| Prop::Compare("<", any.clone(), n.clone()))?; }
                ElementKind::KeptExcept(i) => { let i = term(self, i)?; kept(self, &|any| Prop::Compare("≠", any.clone(), i.clone()))?; }
                ElementKind::Holds(i, item) => {
                    let Some((None, Event::Write(at, Origin::Parameter(stored)))) = frame.log.last() else { return Err(refused("the last thing the body does to the buffer is not a store of the parameter, whole and as it was received, into a cell")) };
                    if stored != item { return Err(refused("the value last stored is another parameter's")); }
                    let i = term(self, i)?;
                    self.shown(WHAT, conjoin(&given, equal(at.clone(), i)), &stated.span)?;
                }
                ElementKind::Among(n) => {
                    let n = term(self, n)?;
                    let mut events = frame.log.iter();
                    while let Some(event) = events.next() {
                        // A cell that may change is one the next thing done stores an old value in.
                        let origin = match event { (None, Event::Except(index)) => match events.next() { Some((None, Event::Write(at, origin))) if at == index => Some(origin), _ => None }, (None, Event::Write(_, origin)) => Some(origin), _ => None };
                        let Some(Origin::Old(from)) = origin else { return Err(refused("the body does something to the buffer other than storing, in a cell, a value read from a cell before anything was stored")) };
                        self.shown(WHAT, conjoin(&given, Prop::Compare("<", from.clone(), n.clone())), &stated.span)?;
                    }
                }
            }
        }
        Ok(())
    }
    /// Every invariant over the same elements as this one (itself among them): a type may
    /// state several over one container, and an element read below their count satisfies each.
    fn siblings(&self, at: usize) -> Vec<usize> {
        // The same cells and the same count: what shows the index of one shows it of each.
        let (root, buffer, length) = (self.watched[at].root, &self.watched[at].buffer, &self.watched[at].length);
        self.watched.iter().enumerate().filter(|(_, watched)| watched.root == root && watched.buffer == *buffer && watched.length == *length).map(|(at, _)| at).collect()
    }
    /// Which invariant's elements this expression reads one of, when it is `v[i]` or a read of
    /// a slice made from `v`'s counted view: the read only returns when the index is below the
    /// count.
    fn element_source(&self, e: &Expr) -> Option<usize> {
        let of = |place: &Place, buffer: &[u32]| {
            if self.lent.contains(&place.root) { return None; }
            let buffer = join(&fields(place)?, buffer);
            self.watched.iter().position(|watched| watched.root == place.root && watched.buffer == buffer)
        };
        match &e.kind {
            Value::Call(call) => {
                let (buffer, _) = storage(self.functions, call.definition.0)?;
                of(box_place(call.args.first()?)?, &buffer)
            }
            Value::SliceIndex(slice, _, _) | Value::SliceAt(slice, _, Reach::Trap | Reach::Walk) => match &slice.kind {
                Value::ReadPlace(place) | Value::Move { place, .. } if place.projections.is_empty() && !place.pointer => self.tags.slices.get(&place.root).copied(),
                Value::Call(call) => {
                    let (buffer, _) = view_storage(self.functions, call.definition.0)?;
                    of(box_place(call.args.first()?)?, &buffer)
                }
                _ => None,
            },
            _ => None,
        }
    }
    /// A local was given a value: an element read into it brings the invariant's clause, a
    /// slice of a watched container is remembered as one, and what a call said of its result
    /// (when that is no `Result`) is said of the local.
    pub(super) fn element_set(&mut self, local: u32, e: &Expr) -> Result<()> {
        if self.watched.is_empty() && self.declared.is_empty() { return Ok(()); }
        if let Value::Call(call) = &e.kind && let Some((buffer, _)) = view_storage(self.functions, call.definition.0)
            && let Some(place) = call.args.first().and_then(box_place) && let Some(base) = fields(place) && !self.lent.contains(&place.root) {
            let buffer = join(&base, &buffer);
            if let Some(at) = self.watched.iter().position(|watched| watched.root == place.root && watched.buffer == buffer) { self.tags.slices.insert(local, at); }
            return Ok(());
        }
        if let Some(found) = self.pending.get(&local).map(|outcome| outcome.elements.clone()) { self.element_arm(&found, None, Some(local), &[])?; }
        let Some(at) = self.element_source(e) else { return Ok(()) };
        for at in self.siblings(at) {
            let watched = self.watched[at].clone();
            let holds = self.instance(&watched, &mut Element::Place((local, Vec::new())), None)?;
            self.hypotheses.push(holds);
        }
        Ok(())
    }
    /// One number field of an element read where it is used (`v[i].part`): the element's
    /// fields get names of their own, and the clause is a fact of them.
    pub(super) fn element_part(&mut self, e: &Expr) -> Result<Option<Math>> {
        if self.watched.is_empty() { return Ok(None); }
        let (mut inner, mut path) = (e, Vec::new());
        while let Value::Project(base, ordinal) = &inner.kind { path.insert(0, *ordinal); inner = base; }
        if path.is_empty() { return Ok(None); }
        let Some(at) = self.element_source(inner) else { return Ok(None) };
        self.effects(inner)?;
        let mut any = Element::Any(BTreeMap::new());
        for at in self.siblings(at) {
            let watched = self.watched[at].clone();
            let holds = self.instance(&watched, &mut any, None)?;
            self.hypotheses.push(holds);
        }
        let Element::Any(mut names) = any else { unreachable!() };
        Ok(Some(match names.remove(&path) { Some(name) => name, None => self.fresh(e.ty) }))
    }
}

/// Whether the call may change `root`: it is lent it, or part of it, to write.
fn changes(arguments: &[Argument], root: u32) -> bool {
    arguments.iter().any(|argument| matches!(argument, Argument::Place { root: other, changed: true, .. } if *other == root))
}
