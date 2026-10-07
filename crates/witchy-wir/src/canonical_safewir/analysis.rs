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

    fn compute(module: &CanonicalModule<InitialValid>, function: &Function) -> Self::Result {
        redundant_bounds_guards(function, &raw_byte_layouts(module))
    }

    fn fact_at(result: &Self::Result, point: ProgramPoint) -> Option<&Self::Fact> {
        result.get(&(point.block, point.operation))
    }
}

/// [`BoundsGuardRedundancy`]'s facts for one function: each bounds or
/// arithmetic guard, keyed by its block and operation index, and whether an
/// earlier guard already implies it on every path. A syntactic fact of the body.
pub(super) fn redundant_bounds_guards(
    function: &Function,
    raw_bytes: &BTreeSet<crate::layout::LayoutId>,
) -> BTreeMap<(BlockId, usize), bool> {
    implied_guards(function, raw_bytes, |_| true)
}

/// The counted layouts whose elements are raw bytes, resolved as the memory
/// checker resolves them: a `Size` element read of one is at most 255.
pub(super) fn raw_byte_layouts<S: super::ModuleStage>(module: &CanonicalModule<S>) -> BTreeSet<crate::layout::LayoutId> {
    let Ok((_, layouts)) = crate::layout::LayoutBundle::decode_canonical(&module.layouts().canonical_bytes()) else {
        return BTreeSet::new();
    };
    module.functions().iter()
        .flat_map(|function| function.value_types.values())
        .filter_map(|value_type| match value_type { ValueType::Ref(layout) => Some(*layout), _ => None })
        .filter(|layout| {
            layouts.resolve_nominal(*layout).ok()
                .and_then(|physical| layouts.get(physical))
                .and_then(|descriptor| descriptor.kind().packed_element_layout())
                .and_then(|element| layouts.resolve_nominal(element).ok().and_then(|element| layouts.get(element)))
                .is_some_and(|element| element.kind() == &crate::layout::LayoutKind::Scalar(crate::layout::ScalarKind::RawByte))
        })
        .collect()
}

/// A guard operand: an SSA value, or a `Size` constant compared by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Operand {
    Value(ValueId),
    Constant(u64),
}

type Relation = (ComparisonOperator, Operand, Operand);

/// What a path has proven about scalar values, mirroring the facts the memory
/// checker derives: `Size` relations from guards and from true `<` branches,
/// upper bounds from true comparisons with a constant, raw-byte loads and
/// width changes, and `Int` bounds against a widened `Size` that narrowing
/// turns into a `Size` relation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Facts {
    relations: BTreeSet<Relation>,
    upper: BTreeMap<ValueId, u64>,
    int_bounds: BTreeSet<(ComparisonOperator, ValueId, ValueId)>,
}

impl Facts {
    fn meet(&self, other: &Self) -> Self {
        Self {
            relations: self.relations.intersection(&other.relations).copied().collect(),
            upper: self.upper.iter()
                .filter_map(|(value, bound)| other.upper.get(value).map(|other| (*value, (*bound).max(*other))))
                .collect(),
            int_bounds: self.int_bounds.intersection(&other.int_bounds).copied().collect(),
        }
    }

    /// The tightest known upper bound of `value`: its recorded bound, a
    /// relation against a constant, or one `extra` derives.
    fn upper_of(&self, value: ValueId, extra: &BTreeMap<ValueId, u64>) -> Option<u64> {
        let relational = self.relations.iter().filter_map(|(operator, left, right)| match (left, right) {
            (Operand::Value(left), Operand::Constant(limit)) if *left == value => match operator {
                ComparisonOperator::LessUnsigned => limit.checked_sub(1),
                ComparisonOperator::LessEqualUnsigned => Some(*limit),
                _ => None,
            },
            _ => None,
        });
        self.upper.get(&value).copied().into_iter()
            .chain(relational)
            .chain(extra.get(&value).copied())
            .min()
    }

    fn bound(&mut self, value: ValueId, upper: u64) {
        let upper = self.upper.get(&value).map_or(upper, |bound| (*bound).min(upper));
        self.upper.insert(value, upper);
    }

    /// Facts that survive an edge passing `arguments` to `parameters`. The checker carries
    /// a fact onto a block parameter only in some cases (never onto a joined
    /// counted lane, which the lowerer re-guards), so the parameters start
    /// unproven, and a fact about a passed argument, which the checker
    /// renames to its parameter, is dropped.
    fn across(&self, arguments: &[ValueId], parameters: &[ValueId]) -> Self {
        // A parameter is a new value on this edge (a loop header reached
        // again): facts about its previous value do not describe it.
        let kept = |value: &ValueId| !arguments.contains(value) && !parameters.contains(value);
        let operand = |operand: &Operand| match operand {
            Operand::Value(value) => kept(value),
            Operand::Constant(_) => true,
        };
        Self {
            relations: self.relations.iter().filter(|(_, left, right)| operand(left) && operand(right)).copied().collect(),
            upper: self.upper.iter().filter(|(value, _)| kept(value)).map(|(value, bound)| (*value, *bound)).collect(),
            int_bounds: self.int_bounds.iter().filter(|(_, left, right)| kept(left) && kept(right)).copied().collect(),
        }
    }
}

/// Which guards are implied where they stand: every path to the guard has
/// proven its relation (or a strict one for `<=`), or bounds its left operand
/// below a constant right operand. A relation true by its operands alone
/// (`h <= h`) still counts as a guard: the checker records it, e.g. to
/// reauthenticate a header it bounds. The checker re-proves the rewritten
/// body. `contributes` selects which guards establish facts, so the rewrite
/// checker can ask the same question of the surviving guards alone.
pub(super) fn implied_guards(
    function: &Function,
    raw_bytes: &BTreeSet<crate::layout::LayoutId>,
    contributes: impl Fn((BlockId, usize)) -> bool,
) -> BTreeMap<(BlockId, usize), bool> {
    let mut constants = BTreeMap::new();
    let mut comparisons = BTreeMap::new();
    let mut widened = BTreeMap::new();
    for block in &function.blocks {
        for operation in &block.operations {
            match operation {
                Operation::ConstSize { destination, value } => { constants.insert(*destination, *value); }
                Operation::ConstInt { destination, value } => {
                    if let Ok(value) = u64::try_from(*value) { constants.insert(*destination, value); }
                }
                Operation::Compare { destination, operator, left, right } => {
                    comparisons.insert(*destination, (*operator, *left, *right));
                }
                Operation::WidenSize { destination, value } => { widened.insert(*destination, *value); }
                _ => {}
            }
        }
    }
    let operand = |value: ValueId| constants.get(&value).map_or(Operand::Value(value), |constant| Operand::Constant(*constant));
    let value_type = |value: ValueId| function.value_types.get(&value).copied();
    let size_relation = |condition: ValueId, operators: &[ComparisonOperator]| -> Option<Relation> {
        let (operator, left, right) = *comparisons.get(&condition)?;
        (operators.contains(&operator) && value_type(left) == Some(ValueType::Size) && value_type(right) == Some(ValueType::Size))
            .then(|| (operator, operand(left), operand(right)))
    };
    let guarded = |operation: &Operation| match operation {
        Operation::Guard { condition, kind: GuardKind::Bounds | GuardKind::Arithmetic } => size_relation(
            *condition, &[ComparisonOperator::LessUnsigned, ComparisonOperator::LessEqualUnsigned]),
        _ => None,
    };
    // An operation after which the checker no longer trusts an earlier bounds
    // guard: it advances an owner generation without rebasing that guard.
    // Relations against a constant bound no owner and survive.
    let invalidates = |operation: &Operation| match operation {
        Operation::UpdateCount { .. } | Operation::Reclaim { .. } => true,
        Operation::Load { transfer, .. } => matches!(transfer,
            super::LoadTransfer::TakeLast | super::LoadTransfer::TakeNullableUnique | super::LoadTransfer::TakeOwnedField),
        Operation::Call { arguments, .. } | Operation::VarCall { arguments, .. } => arguments.iter().any(|argument| matches!(argument,
            super::CallArgument::Move(value) if matches!(value_type(*value), Some(ValueType::Ref(_))))),
        _ => false,
    };
    // The facts each operation adds, guards aside.
    let step = |facts: &mut Facts, operation: &Operation| match operation {
        operation if invalidates(operation) => {
            facts.relations.retain(|(_, _, right)| matches!(right, Operand::Constant(_)));
        }
        Operation::Load { destination, place, .. }
            if matches!(place.projection(), super::Projection::Element { .. })
                && place.value_type() == ValueType::Size
                && matches!(value_type(place.object()), Some(ValueType::Ref(layout)) if raw_bytes.contains(&layout)) =>
        {
            facts.bound(*destination, 255);
        }
        Operation::WidenSize { destination, value } => {
            if let Some(upper) = facts.upper.get(value).copied() { facts.bound(*destination, upper); }
        }
        Operation::NarrowSize { destination, value, .. } => {
            if let Some(upper) = facts.upper.get(value).copied() { facts.bound(*destination, upper.min(u64::from(u32::MAX))); }
            let narrowed = facts.int_bounds.iter().filter(|(_, left, _)| left == value)
                .map(|(operator, _, size)| (*operator, Operand::Value(*destination), operand(*size)))
                .collect::<Vec<_>>();
            facts.relations.extend(narrowed);
        }
        _ => {}
    };
    // The facts a true branch on `condition` adds.
    let assume = |facts: &mut Facts, condition: ValueId| {
        let Some(&(operator, left, right)) = comparisons.get(&condition) else { return };
        let strict = match operator {
            ComparisonOperator::LessUnsigned => true,
            ComparisonOperator::LessEqualUnsigned => false,
            _ => return,
        };
        let kind = value_type(left);
        if !matches!(kind, Some(ValueType::Int | ValueType::Size)) || value_type(right) != kind { return; }
        if let Some(limit) = constants.get(&right).copied()
            && let Some(upper) = if strict { limit.checked_sub(1) } else { Some(limit) }
        {
            facts.bound(left, upper);
        }
        if kind == Some(ValueType::Int) && let Some(size) = widened.get(&right) {
            facts.int_bounds.insert((operator, left, *size));
        }
        if strict && kind == Some(ValueType::Size) {
            facts.relations.insert((operator, operand(left), operand(right)));
        }
    };
    let index: BTreeMap<BlockId, usize> = function.blocks.iter().enumerate().map(|(at, block)| (block.id, at)).collect();
    // Facts on entry to each block; `None` until some predecessor reaches it.
    let mut entry: Vec<Option<Facts>> = vec![None; function.blocks.len()];
    let Some(&start) = index.get(&function.entry) else { return BTreeMap::new() };
    entry[start] = Some(Facts::default());
    let mut pending = std::collections::VecDeque::from([start]);
    while let Some(at) = pending.pop_front() {
        let block = &function.blocks[at];
        let mut facts = entry[at].clone().expect("a pending block has entry facts");
        for (position, operation) in block.operations.iter().enumerate() {
            if let Some(fact) = guarded(operation) && contributes((block.id, position)) {
                facts.relations.insert(fact);
            }
            step(&mut facts, operation);
        }
        let edges: Vec<(BlockId, &[ValueId], Option<ValueId>)> = match &block.terminator {
            Terminator::Branch { target, arguments } => vec![(*target, arguments, None)],
            Terminator::BranchIf { condition, then_target, then_arguments, else_target, else_arguments } => vec![
                (*then_target, then_arguments, Some(*condition)),
                (*else_target, else_arguments, None),
            ],
            _ => Vec::new(),
        };
        for (target, arguments, condition) in edges {
            let Some(&target) = index.get(&target) else { continue };
            let mut outgoing = facts.clone();
            if let Some(condition) = condition { assume(&mut outgoing, condition); }
            let parameters = function.blocks[target].parameters.iter().map(|parameter| parameter.value).collect::<Vec<_>>();
            let outgoing = if arguments.is_empty() && parameters.is_empty() { outgoing } else { outgoing.across(arguments, &parameters) };
            let met = match &entry[target] {
                None => outgoing,
                Some(previous) => previous.meet(&outgoing),
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
            if let Some(fact) = guarded(operation) {
                let (operator, left, right) = fact;
                let below_constant = match (left, right) {
                    (Operand::Value(left), Operand::Constant(limit)) => facts.upper.get(&left).is_some_and(|upper| match operator {
                        ComparisonOperator::LessUnsigned => *upper < limit,
                        _ => *upper <= limit,
                    }),
                    _ => false,
                };
                let holds = below_constant
                    || facts.relations.contains(&fact)
                    || (operator == ComparisonOperator::LessEqualUnsigned
                        && facts.relations.contains(&(ComparisonOperator::LessUnsigned, left, right)));
                implied.insert((block.id, position), holds);
                if contributes((block.id, position)) {
                    facts.relations.insert(fact);
                }
            }
            // A narrowing whose value is known to fit `u32`.
            if let Operation::NarrowSize { value, exact: false, .. } = operation {
                let fits = facts.int_bounds.iter().any(|(_, int, _)| int == value)
                    || facts.upper_of(*value, &widened_size_bounds(&facts)).is_some_and(|upper| upper <= u64::from(u32::MAX));
                implied.insert((block.id, position), fits);
            }
            step(&mut facts, operation);
        }
        // A conditional branch whose condition every path has already
        // proven, keyed at the terminator's position.
        if let Terminator::BranchIf { condition, .. } = &block.terminator
            && let Some(&(operator, left, right)) = comparisons.get(condition)
            && matches!(value_type(left), Some(ValueType::Int | ValueType::Size))
            && value_type(right) == value_type(left)
            && let Some(limit) = constants.get(&right).copied()
        {
            let holds = facts.upper_of(left, &widened_size_bounds(&facts)).is_some_and(|upper| match operator {
                ComparisonOperator::LessUnsigned => upper < limit,
                ComparisonOperator::LessEqualUnsigned => upper <= limit,
                _ => false,
            });
            implied.insert((block.id, block.operations.len()), holds);
        }
    }
    implied
}

/// Upper bounds an `Int` inherits from a proven `int < widen(size)`
/// (or `<=`) and the size's own upper bound.
fn widened_size_bounds(facts: &Facts) -> BTreeMap<ValueId, u64> {
    let mut bounds = BTreeMap::new();
    for (operator, int, size) in &facts.int_bounds {
        let Some(upper) = facts.upper_of(*size, &BTreeMap::new()) else { continue };
        let upper = if *operator == ComparisonOperator::LessUnsigned { upper.saturating_sub(1) } else { upper };
        let bound = bounds.entry(*int).or_insert(upper);
        *bound = (*bound).min(upper);
    }
    bounds
}
