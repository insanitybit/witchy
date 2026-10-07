//! Disposable optimizer knowledge for one immutable SafeWIR revision.
//!
//! An analysis never grants validation authority. The database borrows its
//! module, so mutation requires ending that borrow and constructing a new
//! database. Even unrelated modules with equal revision numbers cannot share
//! cached results. Each analysis computes once per function and answers queries
//! at individual program points without copying a whole fact state per query.

use std::any::{Any, TypeId};
use std::collections::{BTreeMap, BTreeSet};

use super::{
    BlockId, CanonicalModule, ComparisonOperator, Function, FunctionId, GuardKind,
    InitialValid, Operation, Revision, Terminator, ValueId, ValueType,
};

/// The point immediately before an operation, or the terminator when
/// `operation` equals the block's operation count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProgramPoint {
    pub revision: Revision,
    pub function: FunctionId,
    pub block: BlockId,
    pub operation: usize,
}

/// Extend optimization knowledge without adding semantic fields to SafeWIR.
/// Implementations must derive results from the supplied module alone. They
/// may use any internal representation; only `fact_at` exposes scoped facts.
pub trait Analysis: 'static {
    type Result: 'static;
    type Fact: ?Sized;
    const VERSION: u32;

    fn compute(module: &CanonicalModule<InitialValid>, function: &Function) -> Self::Result;
    fn fact_at(result: &Self::Result, point: ProgramPoint) -> Option<&Self::Fact>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct AnalysisKey {
    revision: Revision,
    function: FunctionId,
    implementation: TypeId,
    version: u32,
}

pub struct AnalysisDb<'module> {
    module: &'module CanonicalModule<InitialValid>,
    results: BTreeMap<AnalysisKey, Box<dyn Any>>,
}

impl<'module> AnalysisDb<'module> {
    pub fn new(module: &'module CanonicalModule<InitialValid>) -> Self {
        Self { module, results: BTreeMap::new() }
    }

    pub fn revision(&self) -> Revision {
        self.module.revision()
    }

    /// Unknown or stale points yield no fact. Absence of an analysis fact must
    /// leave the original operation in place, including its runtime checks.
    pub fn query<A: Analysis>(&mut self, point: ProgramPoint) -> Option<&A::Fact> {
        if point.revision != self.module.revision() {
            return None;
        }
        let function = self.module.functions().iter().find(|f| f.id == point.function)?;
        let block = function.blocks.iter().find(|b| b.id == point.block)?;
        if point.operation > block.operations.len() {
            return None;
        }
        let key = AnalysisKey {
            revision: point.revision,
            function: point.function,
            implementation: TypeId::of::<A>(),
            version: A::VERSION,
        };
        let result = self.results.entry(key)
            .or_insert_with(|| Box::new(A::compute(self.module, function)));
        A::fact_at(result.downcast_ref::<A::Result>()?, point)
    }
}

/// Whether a bounds or arithmetic guard is already implied on every path to
/// it (`implied_guards`). This analysis proposes deletions. The separate
/// rewrite checker verifies them and the checker re-proves the module.
pub struct BoundsGuardRedundancy;

impl Analysis for BoundsGuardRedundancy {
    type Result = BTreeMap<(BlockId, usize), bool>;
    type Fact = bool;
    const VERSION: u32 = 1;

    fn compute(_module: &CanonicalModule<InitialValid>, function: &Function) -> Self::Result {
        redundant_bounds_guards(function)
    }

    fn fact_at(result: &Self::Result, point: ProgramPoint) -> Option<&Self::Fact> {
        result.get(&(point.block, point.operation))
    }
}

/// [`BoundsGuardRedundancy`]'s facts for one function: each bounds or
/// arithmetic guard, keyed by its block and operation index, and whether an
/// earlier guard already implies it on every path. A syntactic fact of the body.
pub(super) fn redundant_bounds_guards(function: &Function) -> BTreeMap<(BlockId, usize), bool> {
    implied_guards(function, |_| true)
}

/// A guard operand: an SSA value, or a `Size` constant compared by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Operand {
    Value(ValueId),
    Constant(u64),
}

type Relation = (ComparisonOperator, Operand, Operand);

/// Which guards are implied where they stand. A relation between SSA `Size`
/// values never stops holding once a guard has established it, so a guard
/// is implied when every path to it passed a guard (or the true edge of a
/// branch on a `<` comparison) establishing the same relation, a strict one
/// for `<=`. These are
/// the facts the memory checker itself carries; it still re-proves the
/// rewritten body. `contributes` selects which guards establish facts, so the
/// rewrite checker can ask the same question of the surviving guards alone.
pub(super) fn implied_guards(
    function: &Function,
    contributes: impl Fn((BlockId, usize)) -> bool,
) -> BTreeMap<(BlockId, usize), bool> {
    let mut constants = BTreeMap::new();
    let mut comparisons = BTreeMap::new();
    for block in &function.blocks {
        for operation in &block.operations {
            match operation {
                Operation::ConstSize { destination, value } => { constants.insert(*destination, *value); }
                Operation::Compare { destination, operator, left, right } => {
                    comparisons.insert(*destination, (*operator, *left, *right));
                }
                _ => {}
            }
        }
    }
    let operand = |value: ValueId| constants.get(&value).map_or(Operand::Value(value), |constant| Operand::Constant(*constant));
    let size = |value: ValueId| function.value_types.get(&value) == Some(&ValueType::Size);
    let relation = |condition: ValueId, operators: &[ComparisonOperator]| -> Option<Relation> {
        let (operator, left, right) = *comparisons.get(&condition)?;
        (operators.contains(&operator) && size(left) && size(right)).then(|| (operator, operand(left), operand(right)))
    };
    let guarded = |operation: &Operation| match operation {
        Operation::Guard { condition, kind: GuardKind::Bounds | GuardKind::Arithmetic } => relation(
            *condition, &[ComparisonOperator::LessUnsigned, ComparisonOperator::LessEqualUnsigned]),
        _ => None,
    };
    let index: BTreeMap<BlockId, usize> = function.blocks.iter().enumerate().map(|(at, block)| (block.id, at)).collect();
    // Facts on entry to each block; `None` until some predecessor reaches it.
    let mut entry: Vec<Option<BTreeSet<Relation>>> = vec![None; function.blocks.len()];
    let Some(&start) = index.get(&function.entry) else { return BTreeMap::new() };
    entry[start] = Some(BTreeSet::new());
    let mut pending = std::collections::VecDeque::from([start]);
    while let Some(at) = pending.pop_front() {
        let block = &function.blocks[at];
        let mut facts = entry[at].clone().expect("a pending block has entry facts");
        for (position, operation) in block.operations.iter().enumerate() {
            if let Some(fact) = guarded(operation) && contributes((block.id, position)) {
                facts.insert(fact);
            }
        }
        let edges: Vec<(BlockId, Option<Relation>)> = match &block.terminator {
            Terminator::Branch { target, .. } => vec![(*target, None)],
            Terminator::BranchIf { condition, then_target, else_target, .. } => vec![
                (*then_target, relation(*condition, &[ComparisonOperator::LessUnsigned])),
                (*else_target, None),
            ],
            _ => Vec::new(),
        };
        for (target, edge_fact) in edges {
            let Some(&target) = index.get(&target) else { continue };
            let mut outgoing = facts.clone();
            outgoing.extend(edge_fact);
            let met = match &entry[target] {
                None => outgoing,
                Some(previous) => previous.intersection(&outgoing).copied().collect(),
            };
            if entry[target].as_ref() != Some(&met) {
                entry[target] = Some(met);
                pending.push_back(target);
            }
        }
    }
    let mut implied = BTreeMap::new();
    for (at, block) in function.blocks.iter().enumerate() {
        let Some(mut facts) = entry[at].clone() else { continue };
        for (position, operation) in block.operations.iter().enumerate() {
            let Some(fact) = guarded(operation) else { continue };
            // Only an established relation counts. A relation true by its
            // operands alone (`h <= h`, two constants) is still a guard the
            // checker records, e.g. to reauthenticate a header it bounds.
            let (operator, left, right) = fact;
            let holds = facts.contains(&fact)
                || (operator == ComparisonOperator::LessEqualUnsigned
                    && facts.contains(&(ComparisonOperator::LessUnsigned, left, right)));
            implied.insert((block.id, position), holds);
            if contributes((block.id, position)) {
                facts.insert(fact);
            }
        }
    }
    implied
}
