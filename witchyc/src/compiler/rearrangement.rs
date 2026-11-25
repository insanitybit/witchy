//! What a function does to the values it is given when all it does is move them around
//! (docs/wasm-exchange.md, "A helper that moves an owner").
//!
//! `Option.take`, `replace` and their kind are a few moves, one `memory.exchange` and a
//! variant built from nothing. For such a body the caller's proof need not forget what it knew
//! of the values: each is still the value it was, in another place. The summary says where:
//! for the result and for what each `&mut` parameter points at after the call, which value
//! given to the call is there, or which variant was built around one.
//!
//! It is read from the checked body, by its definition and never by its name, and only from a
//! body that is exactly a straight line of the statements below. Anything else (a branch, a
//! loop, arithmetic, a field, an assignment) has no summary, and a caller then forgets what it
//! lent, as before. A call in such a body is followed when the function called has a summary
//! itself: its summary is composed, each value it was given being the piece this body held
//! there. A call of anything else, a call that could reach the body again, and a chain of
//! calls deeper than `DEPTH` have none. A summary moves what a proof knows; it grants no authority: the
//! ownership checker has already decided that the body may do each step.
use super::logical_body::{Expr, Function, InstanceCall, Stmt, Ty, Value};
use super::logical_place::Place;

/// A value after the call, in terms of the values the call was given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Piece {
    /// The value this parameter held on entry: what a `&mut` parameter pointed at, or the
    /// argument itself for one passed by value.
    Given(u32),
    /// A variant built in the body around a piece.
    Variant(u8, Box<Piece>),
    /// `()`: nothing to know.
    Unit,
}
impl Piece {
    pub(super) fn given(&self, out: &mut Vec<u32>) {
        match self { Piece::Given(index) => if !out.contains(index) { out.push(*index) }, Piece::Variant(_, inner) => inner.given(out), Piece::Unit => {} }
    }
    /// This piece of a callee's summary, in the caller's terms: each value the callee was given
    /// is the piece the caller gave it. `None` when one is named that the caller did not give.
    fn composed(&self, given: &[Option<Piece>]) -> Option<Piece> {
        Some(match self {
            Piece::Given(index) => given.get(*index as usize)?.clone()?,
            Piece::Variant(arm, inner) => Piece::Variant(*arm, Box::new(inner.composed(given)?)),
            Piece::Unit => Piece::Unit,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Summary {
    /// For each parameter: what a `&mut` one points at when the function returns (`None` for
    /// the others).
    pub after: Vec<Option<Piece>>,
    pub result: Piece,
}
impl Summary {
    /// The parameters whose value on entry appears somewhere after the call.
    pub(super) fn given(&self) -> Vec<u32> {
        let mut out = Vec::new();
        for piece in self.after.iter().flatten() { piece.given(&mut out); }
        self.result.given(&mut out);
        out
    }
}

/// The most statements a body may have: these helpers are three or four lines.
const STATEMENTS: usize = 12;
/// The most bodies followed one inside another: a helper, the helper it calls, and so on.
const DEPTH: usize = 4;
/// The most bodies read for one summary, however they are nested.
const BODIES: usize = 32;

/// The summary of `functions[index]`, or `None` when its body is anything but a rearrangement.
pub(super) fn summary(functions: &[Function], index: usize) -> Option<Summary> {
    let mut read = 0;
    summarise(functions, index, &mut Vec::new(), &mut read)
}
/// `open`: the functions whose bodies are being read, outermost first. One that is called
/// again from inside itself has no summary (its effect is not a fixed rearrangement that this
/// reads), and neither has anything that calls it.
fn summarise(functions: &[Function], index: usize, open: &mut Vec<usize>, read: &mut usize) -> Option<Summary> {
    let function = functions.get(index)?;
    if open.contains(&index) || open.len() >= DEPTH || *read >= BODIES { return None; }
    *read += 1;
    if function.required || function.suspends || function.body.is_empty() || function.body.len() > STATEMENTS { return None; }
    let parameters = function.params.len();
    // What each local holds; a `&mut` parameter's entry is what it points at.
    let mut held: Vec<Option<Piece>> = vec![None; function.locals.len()];
    for (index, ty) in function.params.iter().enumerate() {
        match ty {
            Ty::Borrow { mutable: true, .. } => held[index] = Some(Piece::Given(index as u32)),
            // A shared reference is only read: a body that touches one is not followed.
            Ty::Borrow { mutable: false, .. } => {}
            _ => held[index] = Some(Piece::Given(index as u32)),
        }
    }
    let reference = |id: u32| matches!(function.params.get(id as usize), Some(Ty::Borrow { .. }));
    let unique = |id: u32| matches!(function.params.get(id as usize), Some(Ty::Borrow { mutable: true, .. }));
    // A whole local that is not a reference, or the whole of what a `&mut` parameter points at.
    let cell = |place: &Place| -> Option<usize> {
        if !place.projections.is_empty() || place.root as usize >= function.locals.len() { return None; }
        let local = !matches!(function.locals[place.root as usize].ty, Ty::Borrow { .. });
        ((place.pointer && unique(place.root)) || (!place.pointer && local && !reference(place.root))).then_some(place.root as usize)
    };
    fn value(e: &Expr, held: &mut [Option<Piece>], cell: &dyn Fn(&Place) -> Option<usize>) -> Option<Piece> {
        match &e.kind {
            Value::Unit if e.ty == Ty::Unit => Some(Piece::Unit),
            // Moved out of a local: the local holds nothing after. (Never out of what a
            // reference points at; the checker refuses that, and so does this.)
            Value::Move { place, .. } if !place.pointer => { let at = cell(place)?; held[at].take() }
            // A copy of a local leaves it as it was.
            Value::ReadPlace(place) if !place.pointer && !matches!(e.ty, Ty::Borrow { .. }) => { let at = cell(place)?; held[at].clone() }
            Value::Sum { arm, payload } => Some(Piece::Variant(*arm, Box::new(value(payload, held, cell)?))),
            _ => None,
        }
    }
    // A call of a function that has a summary: what it leaves where each `&mut` argument
    // points, and its result, in this body's pieces. Each argument is a value as above, or a
    // `&mut` to a whole place as `memory.exchange` takes one (`&mut local`, `&mut *parameter`,
    // or the `&mut` parameter itself passed on). Anything else in a call, a shared reference
    // among them, is not followed.
    let call = |call: &InstanceCall, held: &mut Vec<Option<Piece>>, open: &mut Vec<usize>, read: &mut usize| -> Option<Piece> {
        let callee = functions.get(call.definition.0 as usize)?;
        if call.args.len() != callee.params.len() { return None; }
        open.push(index);
        let inner = summarise(functions, call.definition.0 as usize, open, read);
        open.pop();
        let inner = inner?;
        if inner.after.len() != call.args.len() { return None; }
        // By-value arguments, in order, as they are evaluated; then where each `&mut` points.
        let mut given: Vec<Option<Piece>> = vec![None; call.args.len()];
        let mut lent: Vec<Option<usize>> = vec![None; call.args.len()];
        for (at, (argument, parameter)) in call.args.iter().zip(&callee.params).enumerate() {
            match (argument.ty, parameter) {
                (Ty::Borrow { mutable: true, .. }, Ty::Borrow { mutable: true, .. }) => {
                    let place = match &argument.kind {
                        Value::Address(place) => cell(place)?,
                        // The reference itself, handed on: what it points at is the place.
                        Value::ReadPlace(place) | Value::Move { place, .. } if place.projections.is_empty() && !place.pointer && unique(place.root) => place.root as usize,
                        _ => return None,
                    };
                    // One place lent twice is refused by the checker; nothing is read from it here.
                    if lent.contains(&Some(place)) { return None; }
                    lent[at] = Some(place);
                }
                (Ty::Borrow { .. }, _) | (_, Ty::Borrow { .. }) => return None,
                _ => given[at] = Some(value(argument, held, &cell)?),
            }
        }
        // What a `&mut` parameter points at is read when the callee runs, after every argument.
        for (at, place) in lent.iter().enumerate() { if let Some(place) = place { given[at] = Some(held[*place].clone()?); } }
        // Every piece is made before any place is written: the callee's summary speaks of the
        // values on its entry.
        let mut left = Vec::new();
        for (at, place) in lent.iter().enumerate() {
            match (place, &inner.after[at]) {
                (Some(place), Some(piece)) => left.push((*place, piece.composed(&given)?)),
                (None, None) => {}
                _ => return None,
            }
        }
        let result = inner.result.composed(&given)?;
        for (place, piece) in left { held[place] = Some(piece); }
        Some(result)
    };
    let mut result = None;
    for (at, statement) in function.body.iter().enumerate() {
        match statement {
            Stmt::Set(id, e) if (*id as usize) >= parameters && (*id as usize) < held.len() && !matches!(e.ty, Ty::Borrow { .. }) => {
                let piece = match &e.kind { Value::Call(inner) => call(inner, &mut held, open, read)?, _ => value(e, &mut held, &cell)? };
                held[*id as usize] = Some(piece);
            }
            Stmt::Eval(Expr { kind: Value::Exchange(left, right), .. }) => {
                let (a, b) = (cell(left)?, cell(right)?);
                if a == b || held[a].is_none() || held[b].is_none() { return None; }
                held.swap(a, b);
            }
            // A call for what it does to the places it is lent: it gives back nothing to keep.
            Stmt::Eval(Expr { kind: Value::Call(inner), ty: Ty::Unit }) => { if call(inner, &mut held, open, read)? != Piece::Unit { return None; } }
            Stmt::Expire(_) => {}
            Stmt::Return(e, _, _) if at + 1 == function.body.len() && !matches!(e.ty, Ty::Borrow { .. }) => result = Some(match &e.kind {
                Value::Call(inner) => call(inner, &mut held, open, read)?,
                _ => value(e, &mut held, &cell)?,
            }),
            _ => return None,
        }
    }
    let result = result?;
    let mut after = vec![None; parameters];
    for (index, ty) in function.params.iter().enumerate() {
        if matches!(ty, Ty::Borrow { mutable: true, .. }) { after[index] = Some(held[index].clone()?); }
    }
    Some(Summary { after, result })
}
