//! Translate callable contracts into scalar clauses over arguments and results.
use super::{BOX_ADDRESS, BOX_ID, Result, equal, read};
use super::super::{
    contract::{Contract, Math, Prop, RESULT},
    logical_body::{Function, Ty},
    memory_contract as memory,
    record_contract as record,
    scalar::Scalar,
};

/// A scalar a callee's contract names: part of an argument (`before` the call or after it), or part of the result.
pub(super) struct Leaf {
    pub(super) parameter: Option<u32>,
    pub(super) fields: Vec<u32>,
    pub(super) ty: Ty,
    pub(super) before: bool,
}
/// A callee's contract as claims about numbers. A name in a clause is an index into `leaves`.
#[derive(Default)]
pub(super) struct Claims {
    pub(super) requires: Vec<Prop>,
    pub(super) ensures: Vec<(Option<u8>, Prop)>,
    pub(super) leaves: Vec<Leaf>,
    /// What the callee does to the cells of buffers it is lent (docs/proposal-element-invariants.md).
    pub(super) elements: Vec<ElementClaim>,
}
/// Whose value a cell holds: a parameter's, or the result (for a `Result`, its variant's value).
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Held { Parameter(u32), Result }
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Effect {
    /// Every cell is as it was.
    Kept,
    /// Every cell below `index` is as it was.
    KeptBelow,
    /// Every cell but the one at `index` is as it was.
    KeptExcept,
    /// Every value below `index` was a value below `index` before.
    Among,
    /// The cell at `index` holds this value after the call.
    Holds(Held),
    /// The cell at `index` held this value before the call.
    Was(Held),
}
/// One such statement: of the buffer at `buffer` under the callee's parameter `parameter`.
pub(super) struct ElementClaim { pub(super) parameter: u32, pub(super) buffer: Vec<u32>, pub(super) arm: Option<u8>, pub(super) effect: Effect, pub(super) index: Option<Math> }
impl Claims {
    fn leaf(&mut self, leaf: Leaf) -> Math { self.leaves.push(leaf); Math::Binding(self.leaves.len() as u32 - 1) }
    /// A scalar contract: every clause, with method calls resolved to the fields they return.
    pub(super) fn scalar(functions: &[Function], callee: &Function, contract: &Contract) -> Result<Self> {
        fn name(claims: &mut Claims, functions: &[Function], callee: &Function, m: &Math) -> Result<Math> {
            Ok(match m {
                Math::Binding(id) => claims.leaf(Leaf { parameter: Some(*id), fields: Vec::new(), ty: callee.locals[*id as usize].ty, before: true }),
                Math::Result => claims.leaf(Leaf { parameter: None, fields: Vec::new(), ty: callee.result, before: false }),
                other => match read(functions, other)? {
                    Some((RESULT, fields, ty, _)) => claims.leaf(Leaf { parameter: None, fields, ty, before: false }),
                    Some((root, fields, ty, before)) => claims.leaf(Leaf { parameter: Some(root), fields, ty, before }),
                    None => other.clone(),
                },
            })
        }
        let mut claims = Self::default();
        for clause in &contract.requires {
            let value = clause.value.rewrite(&mut |m| name(&mut claims, functions, callee, m))?;
            claims.requires.push(value);
        }
        // Before the call there is one state: every name in a `requires` is read there.
        for leaf in &mut claims.leaves { leaf.before = true; }
        for clause in &contract.ensures { let value = clause.value.rewrite(&mut |m| name(&mut claims, functions, callee, m))?; claims.ensures.push((clause.arm, value)); }
        // Its `elements.` clauses, which its own proof showed of its body (elements.rs).
        for stated in &contract.effects {
            use super::super::contract::ElementKind;
            let Some((parameter, buffer, _, false)) = read(functions, &stated.buffer)? else { continue };
            let (effect, index) = match &stated.kind {
                ElementKind::Kept => (Effect::Kept, None),
                ElementKind::KeptBelow(n) => (Effect::KeptBelow, Some(n)),
                ElementKind::KeptExcept(i) => (Effect::KeptExcept, Some(i)),
                ElementKind::Among(n) => (Effect::Among, Some(n)),
                ElementKind::Holds(i, item) => (Effect::Holds(Held::Parameter(*item)), Some(i)),
            };
            let index = match index { Some(index) => Some(index.rewrite(&mut |m| name(&mut claims, functions, callee, m))?), None => None };
            claims.elements.push(ElementClaim { parameter, buffer, arm: stated.arm, effect, index });
        }
        Ok(claims)
    }
    /// A record contract's comparisons of numbers. Claims about memory are left to the record model.
    pub(super) fn record(contract: &record::RecordContract) -> Self {
        fn term(claims: &mut Claims, t: &record::Term, mutable_owner: Option<u32>) -> Option<Math> {
            Some(match t {
                record::Term::Literal(n) => Math::Literal(*n),
                record::Term::Add(a, b) => Math::Add(Box::new(term(claims, a, mutable_owner)?), Box::new(term(claims, b, mutable_owner)?)),
                record::Term::Capacity(_) | record::Term::Bound => return None,
                record::Term::Path(p) => {
                    let parameter = match p.root { record::Root::Parameter(id) => Some(id), record::Root::Arm(_) => None, record::Root::Local(id) if mutable_owner == Some(id) => Some(id), record::Root::Local(_) => return None };
                    claims.leaf(Leaf { parameter, fields: p.fields.iter().map(|f| *f as u32).collect(), ty: p.ty, before: p.before })
                }
            })
        }
        fn clause(claims: &mut Claims, p: &record::Predicate, mutable_owner: Option<u32>) -> Option<Prop> {
            Some(match p {
                record::Predicate::Bool(b) => Prop::Bool(*b),
                record::Predicate::Compare(op, a, b) => Prop::Compare(op, term(claims, a, mutable_owner)?, term(claims, b, mutable_owner)?),
                record::Predicate::Not(inner) => Prop::Not(Box::new(clause(claims, inner, mutable_owner)?)),
                record::Predicate::Binary(op, a, b) => Prop::Binary(op, Box::new(clause(claims, a, mutable_owner)?), Box::new(clause(claims, b, mutable_owner)?)),
                _ => return None,
            })
        }
        let mut claims = Self::default();
        for (arm, predicate) in &contract.ensures {
            if let record::Predicate::Same(a,b) = predicate {
                // A live allocation keeps its address across a transfer of its owner.
                for (step,ty) in [(BOX_ID,Ty::U64),(BOX_ADDRESS,Ty::Unsigned(Scalar::U32))] {
                    let part = |p: &record::Path| {
                        let parameter = match p.root { record::Root::Parameter(id) => Some(id), record::Root::Arm(_) => None, _ => return None };
                        Some(Leaf { parameter, fields: p.fields.iter().map(|i| *i as u32).chain([step]).collect(), ty, before:p.before })
                    };
                    if let (Some(x),Some(y)) = (part(a),part(b)) {
                        let (x,y) = (claims.leaf(x),claims.leaf(y));
                        claims.ensures.push((*arm,equal(x,y)));
                    }
                }
            }
            if let Some(value) = clause(&mut claims, predicate, contract.mutable_owner) { claims.ensures.push((*arm, value)); }
            // What Lean showed of the cells: the same buffer of one parameter, after and before.
            let buffer = |p: &record::Path| match p.root { record::Root::Parameter(id) => Some((id, p.fields.iter().map(|f| *f as u32).collect::<Vec<_>>())),
                record::Root::Local(id) if contract.mutable_owner == Some(id) => Some((id, p.fields.iter().map(|f| *f as u32).collect::<Vec<_>>())), _ => None };
            let same = |a: &record::Path, b: &record::Path| match (buffer(a), buffer(b)) { (Some(x), Some(y)) if x == y && !a.before && b.before => Some(x), _ => None };
            let stated = match predicate {
                record::Predicate::Unchanged(a, b) => same(a, b).map(|at| (at, Effect::Kept, None)),
                record::Predicate::Except(a, b, i) => same(a, b).map(|at| (at, Effect::KeptExcept, Some(i))),
                record::Predicate::EqualPrefix(a, b, n) => same(a, b).map(|at| (at, Effect::KeptBelow, Some(n))),
                record::Predicate::Cell(cells, i, record::Term::Path(item)) if item.fields.is_empty() => buffer(cells).and_then(|at| {
                    let held = match item.root { record::Root::Parameter(id) if item.before || !matches!(item.ty, Ty::Borrow { .. }) => Held::Parameter(id), record::Root::Arm(_) => Held::Result, _ => return None };
                    Some((at, if cells.before { Effect::Was(held) } else { Effect::Holds(held) }, Some(i)))
                }),
                _ => None,
            };
            if let Some(((parameter, buffer), effect, index)) = stated {
                let index = match index { Some(index) => match term(&mut claims, index, contract.mutable_owner) { Some(index) => Some(index), None => continue }, None => None };
                claims.elements.push(ElementClaim { parameter, buffer, arm: *arm, effect, index });
            }
        }
        claims
    }
    /// A memory contract's comparisons of numbers, likewise.
    pub(super) fn memory(contract: &memory::MemoryContract) -> Self {
        fn path(claims: &mut Claims, p: &memory::Path, result: Ty) -> Math {
            // `result.len()` of a returned slice: its length, as a slice local's is keyed.
            if p.root == u32::MAX && p.fields.is_empty() && matches!(result, Ty::Slice(_)) {
                return claims.leaf(Leaf { parameter: None, fields: vec![super::SLICE_LENGTH], ty: Ty::Unsigned(Scalar::U64), before: false });
            }
            claims.leaf(Leaf { parameter: (p.root != u32::MAX).then_some(p.root), fields: p.fields.iter().map(|f| *f as u32).collect(), ty: p.ty, before: p.before })
        }
        fn number(claims: &mut Claims, n: &memory::Number, result: Ty) -> Option<Math> {
            Some(match n {
                memory::Number::Literal(n) => Math::Literal(*n),
                memory::Number::Add(a, b) => Math::Add(Box::new(number(claims, a, result)?), Box::new(number(claims, b, result)?)),
                memory::Number::AllocationKey(_) | memory::Number::Capacity(_) | memory::Number::Bound => return None,
                memory::Number::Path(p) => path(claims, p, result),
            })
        }
        fn clause(claims: &mut Claims, p: &memory::Predicate, result: Ty) -> Option<Prop> {
            Some(match p {
                memory::Predicate::Bool(b) => Prop::Bool(*b),
                memory::Predicate::Compare(op, a, b) => Prop::Compare(op, number(claims, a, result)?, number(claims, b, result)?),
                memory::Predicate::Not(inner) => Prop::Not(Box::new(clause(claims, inner, result)?)),
                memory::Predicate::Binary(op, a, b) => Prop::Binary(op, Box::new(clause(claims, a, result)?), Box::new(clause(claims, b, result)?)),
                memory::Predicate::Result => Prop::Truth(claims.leaf(Leaf { parameter: None, fields: Vec::new(), ty: result, before: false })),
                _ => return None,
            })
        }
        let mut claims = Self::default();
        for (arm, predicate) in &contract.ensures {
            if let Some(value) = clause(&mut claims, predicate, contract.result) { claims.ensures.push((*arm, value)); }
            // What Lean showed of the cells: the same buffer of one parameter, after and before.
            let buffer = |p: &memory::Path| (p.root != u32::MAX).then(|| (p.root, p.fields.iter().map(|f| *f as u32).collect::<Vec<_>>()));
            let same = |a: &memory::Path, b: &memory::Path| match (buffer(a), buffer(b)) { (Some(x), Some(y)) if x == y && !a.before && b.before => Some(x), _ => None };
            let stated = match predicate {
                memory::Predicate::Unchanged(a, b) => same(a, b).map(|at| (at, Effect::Kept, None)),
                memory::Predicate::Except(a, b, i) => same(a, b).map(|at| (at, Effect::KeptExcept, Some(i))),
                memory::Predicate::SamePrefix(a, b, n) => same(a, b).map(|at| (at, Effect::KeptBelow, Some(n))),
                memory::Predicate::Cell(cells, i, item) => buffer(cells).and_then(|at| {
                    let held = match item { memory::Element::Result => Held::Result,
                        // A variant's value, named by the arm's pattern, is rooted as the result is.
                        memory::Element::Path(p) if p.fields.is_empty() && !p.before => if p.root == u32::MAX { Held::Result } else { Held::Parameter(p.root) },
                        _ => return None };
                    Some((at, if cells.before { Effect::Was(held) } else { Effect::Holds(held) }, Some(i)))
                }),
                _ => None,
            };
            if let Some(((parameter, buffer), effect, index)) = stated {
                let index = match index { Some(index) => match number(&mut claims, index, contract.result) { Some(index) => Some(index), None => continue }, None => None };
                claims.elements.push(ElementClaim { parameter, buffer, arm: *arm, effect, index });
            }
        }
        claims
    }
}
