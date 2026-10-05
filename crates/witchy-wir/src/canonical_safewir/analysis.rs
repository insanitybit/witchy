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
    BlockId, CanonicalModule, Function, FunctionId, GuardKind, InitialValid,
    Operation, Revision,
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

/// Whether a bounds guard repeats an earlier bounds guard on the same scalar
/// SSA comparison in this block. Scalar definitions do not change across calls;
/// loaded values and comparisons with different definitions remain distinct.
/// This analysis proposes deletions. The separate rewrite checker verifies them.
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

/// [`BoundsGuardRedundancy`]'s facts for one function: each bounds guard,
/// keyed by its block and operation index, and whether it repeats an earlier
/// guard of the same comparison. A syntactic fact of the body alone.
pub(super) fn redundant_bounds_guards(function: &Function) -> BTreeMap<(BlockId, usize), bool> {
    let mut facts = BTreeMap::new();
    for block in &function.blocks {
        let mut comparisons = BTreeMap::new();
        let mut guarded = BTreeSet::new();
        for (index, operation) in block.operations.iter().enumerate() {
            match operation {
                Operation::Compare { destination, operator, left, right } => {
                    comparisons.insert(*destination, (*operator, *left, *right));
                }
                Operation::Guard { condition, kind: GuardKind::Bounds } => {
                    let redundant = comparisons.get(condition)
                        .is_some_and(|comparison| !guarded.insert(*comparison));
                    facts.insert((block.id, index), redundant);
                }
                _ => {}
            }
        }
    }
    facts
}
