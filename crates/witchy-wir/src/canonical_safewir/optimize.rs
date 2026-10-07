//! Assured canonical SafeWIR transformations.
//!
//! Production passes consume validated input, consult disposable analyses,
//! construct an authority-free candidate, and revalidate it. The driver keeps
//! the original checked input if a pass fails; final validation is still required.
//! The bounds-guard rewrite can also run on a lowered candidate before its first
//! check ([`remove_redundant_bounds_guards_before_check`]); its output is then an
//! ordinary lowered candidate, admitted only by the complete initial check.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use super::analysis::{AnalysisDb, BoundsGuardRedundancy, ProgramPoint};

use super::{
    BlockId, CanonicalModule, CheckedInitialSafeWir, CheckedOptimizedSafeWir, CheckedSafeWirRevision,
    Function, GuardKind, InitialCheckError, InitialValid, LoweredCandidate, ModuleStage,
    Operation, OptimizationAuthorityError, OptimizationCandidate, Revision, Terminator,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundsGuardEliminationError {
    NoChange,
    RevisionOverflow { revision: Revision },
    Authority(OptimizationAuthorityError),
    RewriteRelation(RewriteRelationError),
    Safety(InitialCheckError),
}

impl fmt::Display for BoundsGuardEliminationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoChange => write!(formatter, "bounds-guard pass made no change"),
            Self::RevisionOverflow { revision } => {
                write!(formatter, "cannot advance SafeWIR revision {revision:?}")
            }
            Self::Authority(error) => error.fmt(formatter),
            Self::RewriteRelation(error) => {
                write!(formatter, "bounds-guard rewrite relation failed: {error}")
            }
            Self::Safety(error) => write!(
                formatter,
                "bounds-guard candidate failed validation: {error}"
            ),
        }
    }
}

impl std::error::Error for BoundsGuardEliminationError {}

/// Apply the pass to checked initial authority and bind the exact revalidated
/// output to a private optimization witness for independent final validation.
pub fn eliminate_same_block_redundant_bounds_guards_checked(
    before: CheckedInitialSafeWir,
) -> Result<CheckedOptimizedSafeWir, BoundsGuardEliminationError> {
    let before = CheckedSafeWirRevision::from(before);
    attempt_bounds_guard_pass(&before)
}

fn attempt_bounds_guard_pass(
    before: &CheckedSafeWirRevision,
) -> Result<CheckedOptimizedSafeWir, BoundsGuardEliminationError> {
    let optimized = bounds_guard_candidate(before.module().clone(), before.affine_abi().map(|proofs| &**proofs))?;
    if optimized.revision() == before.module().revision()
        && super::identity::module_digest(&optimized)
            == super::identity::module_digest(before.module())
    {
        return Err(BoundsGuardEliminationError::NoChange);
    }
    super::authority::bind_checked_same_block_bounds_guard_optimization(before, optimized)
        .map_err(BoundsGuardEliminationError::Authority)
}

/// Enabled transformations have explicit input/output and rejection behavior.
/// Adding a pass requires an exhaustive dispatch arm that checks its candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassId {
    SameBlockBoundsGuards,
}

pub enum CheckedPassOutcome {
    Applied(CheckedOptimizedSafeWir),
    Retained {
        current: CheckedSafeWirRevision,
        reason: CheckedPassError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckedPassError {
    SameBlockBoundsGuards(BoundsGuardEliminationError),
}

impl CheckedPassOutcome {
    pub fn module(&self) -> &CanonicalModule<InitialValid> {
        match self {
            Self::Applied(checked) => checked.module(),
            Self::Retained { current, .. } => current.module(),
        }
    }

    pub fn validate_final(
        self,
        source: &witchy_canonical_source::source::CanonicalSourceView<'_>,
    ) -> Result<super::ValidatedSafeWir, super::FinalValidationError> {
        match self {
            Self::Applied(checked) => super::validate_optimized_final(checked, source),
            Self::Retained { current, .. } => current.validate_final(source),
        }
    }
}

trait CheckedPassContract {
    fn apply(
        before: &CheckedSafeWirRevision,
    ) -> Result<CheckedOptimizedSafeWir, CheckedPassError>;
}

struct SameBlockBoundsGuardPass;

impl CheckedPassContract for SameBlockBoundsGuardPass {
    fn apply(
        before: &CheckedSafeWirRevision,
    ) -> Result<CheckedOptimizedSafeWir, CheckedPassError> {
        attempt_bounds_guard_pass(before).map_err(CheckedPassError::SameBlockBoundsGuards)
    }
}

pub fn run_checked_pass(
    before: impl Into<CheckedSafeWirRevision>,
    pass: PassId,
) -> CheckedPassOutcome {
    let before = before.into();
    let result = match pass {
        PassId::SameBlockBoundsGuards => SameBlockBoundsGuardPass::apply(&before),
    };
    match result {
        Ok(after) => CheckedPassOutcome::Applied(after),
        Err(reason) => CheckedPassOutcome::Retained { current: before, reason },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteRelationError {
    RevisionMismatch,
    ModuleMetadataChanged,
    FunctionShapeChanged,
    BlockShapeChanged,
    OperationChanged,
    GuardHasNoLocalComparison,
    GuardHasNoEarlierSurvivingMatch,
}

impl fmt::Display for RewriteRelationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

/// Remove only a later bounds guard whose comparison has exactly the same
/// operator and SSA operands as an earlier surviving bounds guard in the same
/// basic block.
///
/// Production dispatch uses this same analysis and candidate checker.
pub fn eliminate_same_block_redundant_bounds_guards(
    before: CanonicalModule<InitialValid>,
) -> Result<CanonicalModule<InitialValid>, BoundsGuardEliminationError> {
    bounds_guard_candidate(before, None)
}

fn bounds_guard_candidate(
    before: CanonicalModule<InitialValid>,
    affine_abi: Option<&super::check::AffineAbiProofs>,
) -> Result<CanonicalModule<InitialValid>, BoundsGuardEliminationError> {
    let mut candidate: CanonicalModule<OptimizationCandidate> = before.clone().into_stage();
    if rewrite(&mut candidate, &mut AnalysisDb::new(&before)) {
        candidate.revision = before.revision.checked_next().ok_or(
            BoundsGuardEliminationError::RevisionOverflow {
                revision: before.revision,
            },
        )?;
    }
    check_rewrite_relation(&before, &candidate)
        .map_err(BoundsGuardEliminationError::RewriteRelation)?;
    super::check::check_optimization_candidate(candidate, affine_abi)
        .map_err(BoundsGuardEliminationError::Safety)
}

fn rewrite(
    candidate: &mut CanonicalModule<OptimizationCandidate>,
    analyses: &mut AnalysisDb<'_>,
) -> bool {
    let mut changed = false;
    for function in &mut candidate.functions {
        for block in &mut function.blocks {
            let before_len = block.operations.len();
            let mut operation_index = 0;
            block.operations.retain(|operation| {
                let point = ProgramPoint {
                    revision: analyses.revision(),
                    function: function.id,
                    block: block.id,
                    operation: operation_index,
                };
                operation_index += 1;
                if !matches!(operation, Operation::Guard { kind: GuardKind::Bounds | GuardKind::Arithmetic, .. }) {
                    return true;
                }
                analyses.query::<BoundsGuardRedundancy>(point) != Some(&true)
            });
            changed |= block.operations.len() != before_len;
        }
    }
    changed
}

/// The implied-guard rewrite on a lowered candidate, before any check.
/// Nothing here grants authority: the result is an ordinary lowered candidate
/// that must pass the complete initial check, and its shape must match
/// `lowered` less some guards. Checking this module first spares
/// checking the guarded one whenever it is admitted; a caller that sees one
/// function rejected retries with that function left `excluded` (as
/// lowered), and checks `lowered` itself as the last resort. `None` when no
/// guard is implied or the relation fails.
pub fn remove_redundant_bounds_guards_before_check(
    lowered: &CanonicalModule<LoweredCandidate>,
    excluded: &BTreeSet<super::FunctionId>,
) -> Option<CanonicalModule<LoweredCandidate>> {
    // Copy the module only once some guard is known to go.
    let raw_bytes = super::analysis::raw_byte_layouts(lowered);
    let redundant = lowered.functions.iter()
        .map(|function| if excluded.contains(&function.id) { BTreeMap::new() }
            else { super::analysis::redundant_bounds_guards(function, &raw_bytes) })
        .collect::<Vec<_>>();
    if !redundant.iter().any(|facts| facts.values().any(|&redundant| redundant)) {
        return None;
    }
    let mut candidate = lowered.clone();
    for ((function, original), redundant) in candidate.functions.iter_mut().zip(&lowered.functions).zip(&redundant) {
        // A branch folds only on evidence that survives this rewrite, as
        // the relation re-proves it: never on a guard removed alongside it.
        let foldable = foldable_branches(original, redundant, &raw_bytes);
        for block in &mut function.blocks {
            let block_id = block.id;
            // A conditional branch whose condition every path has proven
            // always takes its true edge.
            if foldable.contains(&(block_id, block.operations.len()))
                && let Terminator::BranchIf { then_target, then_arguments, .. } = &mut block.terminator
            {
                let (target, arguments) = (*then_target, std::mem::take(then_arguments));
                block.terminator = Terminator::Branch { target, arguments };
            }
            // A narrowing of a value known to fit needs no runtime test; the
            // checker admits `exact` only where it proves the same.
            for (index, operation) in block.operations.iter_mut().enumerate() {
                if let Operation::NarrowSize { exact, .. } = operation
                    && redundant.get(&(block_id, index)) == Some(&true)
                {
                    *exact = true;
                }
            }
            let mut operation_index = 0;
            // Only a bounds guard: the initial check re-proves memory safety,
            // but nothing re-proves an arithmetic trap the analysis judged
            // implied. The checked pass removes those with its own re-proof.
            block.operations.retain(|operation| {
                let keep = redundant.get(&(block_id, operation_index)) != Some(&true)
                    || !matches!(operation, Operation::Guard { kind: GuardKind::Bounds, .. });
                operation_index += 1;
                keep
            });
        }
        // A folded branch's false edge may have been the only way into its
        // diagnostic; the encoder admits no unreachable block.
        let reachable = reachable_blocks(function);
        function.blocks.retain(|block| reachable.contains(&block.id));
    }
    // A dropped diagnostic may have been the only use of its message.
    let referenced = referenced_static_data(&candidate);
    candidate.static_data_segments.retain(|segment| referenced.contains(&segment.id()));
    check_rewrite_relation_between(lowered, &candidate, false).ok()?;
    Some(candidate)
}

fn referenced_static_data<S: ModuleStage>(module: &CanonicalModule<S>) -> BTreeSet<super::StaticDataId> {
    module.functions.iter().flat_map(|function| &function.blocks).flat_map(|block| &block.operations)
        .filter_map(|operation| match operation {
            Operation::StaticData { data, .. } => Some(*data),
            _ => None,
        })
        .collect()
}

fn foldable_branches(
    function: &Function,
    redundant: &BTreeMap<(BlockId, usize), bool>,
    raw_bytes: &BTreeSet<crate::layout::LayoutId>,
) -> BTreeSet<(BlockId, usize)> {
    let mut removed = BTreeSet::new();
    let mut branches = BTreeSet::new();
    for block in &function.blocks {
        for (index, operation) in block.operations.iter().enumerate() {
            if matches!(operation, Operation::Guard { kind: GuardKind::Bounds, .. })
                && redundant.get(&(block.id, index)) == Some(&true)
            {
                removed.insert((block.id, index));
            }
        }
        let point = (block.id, block.operations.len());
        if matches!(block.terminator, Terminator::BranchIf { .. }) && redundant.get(&point) == Some(&true) {
            branches.insert(point);
        }
    }
    if branches.is_empty() || removed.is_empty() { return branches; }
    let implied = super::analysis::implied_guards(function, raw_bytes, |point| !removed.contains(&point));
    branches.retain(|point| implied.get(point) == Some(&true));
    branches
}

fn reachable_blocks(function: &Function) -> BTreeSet<BlockId> {
    let terminators: BTreeMap<_, _> = function.blocks.iter().map(|block| (block.id, &block.terminator)).collect();
    let mut reachable = BTreeSet::new();
    let mut pending = vec![function.entry];
    while let Some(block) = pending.pop() {
        if !reachable.insert(block) { continue; }
        if let Some(terminator) = terminators.get(&block) {
            pending.extend(super::check::successors(terminator));
        }
    }
    reachable
}

/// `after` keeps exactly the segments of `before` that its code references.
fn retains_referenced_static_data<A: ModuleStage, B: ModuleStage>(
    before: &CanonicalModule<A>,
    after: &CanonicalModule<B>,
) -> bool {
    let referenced = referenced_static_data(after);
    before.static_data_segments.iter().filter(|segment| referenced.contains(&segment.id()))
        .eq(after.static_data_segments.iter())
}

/// Independently recognize the one allowed rewrite. This intentionally does
/// not call `rewrite` or compare against a regenerated candidate.
pub(super) fn check_rewrite_relation(
    before: &CanonicalModule<InitialValid>,
    after: &CanonicalModule<OptimizationCandidate>,
) -> Result<(), RewriteRelationError> {
    check_rewrite_relation_between(before, after, true)
}

/// The relation between any two stages; `advances_revision` says whether a
/// change must advance the revision (a pass over a checked revision) or keep
/// it (a lowered candidate that has not been checked yet). Only a pass over a
/// checked revision re-proves that each removed guard is implied: nothing
/// checks its result again, while a lowered candidate goes through the
/// complete initial check.
fn check_rewrite_relation_between<A: ModuleStage, B: ModuleStage>(
    before: &CanonicalModule<A>,
    after: &CanonicalModule<B>,
    advances_revision: bool,
) -> Result<(), RewriteRelationError> {
    if before.checked_source != after.checked_source
        || before.launch_contract_payload != after.launch_contract_payload
        || before.layouts != after.layouts
        || before.signatures != after.signatures
        || before.imports != after.imports
        || before.proper_tail_sites != after.proper_tail_sites
        || before.proper_tail_locations != after.proper_tail_locations
        || !retains_referenced_static_data(before, after)
        || before.abi_catalog != after.abi_catalog
        || before.abi_bindings != after.abi_bindings
        || before.closure_bindings != after.closure_bindings
        || before.adapter_bindings != after.adapter_bindings
        || before.closure_layouts != after.closure_layouts
        || before.owner_families != after.owner_families
        || before.vm_services != after.vm_services
    {
        return Err(RewriteRelationError::ModuleMetadataChanged);
    }
    if before.functions.len() != after.functions.len() {
        return Err(RewriteRelationError::FunctionShapeChanged);
    }
    let mut changed = false;
    // Decoded only when some guard or branch must be re-proven.
    let raw_bytes = std::cell::OnceCell::new();
    let raw_bytes = || raw_bytes.get_or_init(|| super::analysis::raw_byte_layouts(before));
    for (before, after) in before.functions.iter().zip(&after.functions) {
        changed |= check_function_relation(before, after, &raw_bytes, advances_revision)?;
    }
    let expected_revision = if changed && advances_revision {
        before
            .revision
            .checked_next()
            .ok_or(RewriteRelationError::RevisionMismatch)?
    } else {
        before.revision
    };
    if after.revision != expected_revision {
        return Err(RewriteRelationError::RevisionMismatch);
    }
    Ok(())
}

/// `reprove_guards` says whether each removed guard must be re-proven
/// implied. A folded branch always must be: the checker proves memory
/// safety, not that a diagnostic it no longer sees could never fire.
fn check_function_relation<'r>(
    before: &Function,
    after: &Function,
    raw_bytes: &dyn Fn() -> &'r BTreeSet<crate::layout::LayoutId>,
    reprove_guards: bool,
) -> Result<bool, RewriteRelationError> {
    if before.id != after.id
        || before.signature != after.signature
        || before.entry != after.entry
        || before.value_types != after.value_types
    {
        return Err(RewriteRelationError::FunctionShapeChanged);
    }
    // `after` keeps `before`'s blocks in order, less some that a folded
    // branch left unreachable: `after` names no block it lacks.
    let kept: BTreeSet<_> = after.blocks.iter().map(|block| block.id).collect();
    if !kept.contains(&before.entry) {
        return Err(RewriteRelationError::BlockShapeChanged);
    }
    let mut before_blocks = before.blocks.iter();
    let mut pairs = Vec::with_capacity(after.blocks.len());
    for after in &after.blocks {
        let before = before_blocks.find(|before| before.id == after.id)
            .ok_or(RewriteRelationError::BlockShapeChanged)?;
        if super::check::successors(&after.terminator).iter().any(|target| !kept.contains(target)) {
            return Err(RewriteRelationError::BlockShapeChanged);
        }
        pairs.push((before, after));
    }
    let mut changed = kept.len() != before.blocks.len();
    let mut removed = BTreeSet::new();
    let mut folded = Vec::new();
    let function = before;
    for (before, after) in pairs {
        if before.id != after.id || before.parameters != after.parameters {
            return Err(RewriteRelationError::BlockShapeChanged);
        }
        if before.terminator != after.terminator {
            let (Terminator::BranchIf { then_target, then_arguments, .. }, Terminator::Branch { target, arguments }) =
                (&before.terminator, &after.terminator) else { return Err(RewriteRelationError::BlockShapeChanged) };
            if then_target != target || then_arguments != arguments {
                return Err(RewriteRelationError::BlockShapeChanged);
            }
            folded.push((before.id, before.operations.len()));
            changed = true;
        }

        // `after` keeps every operation of `before` in order, less some
        // guards; record which.
        let mut after_index = 0;
        for (index, operation) in before.operations.iter().enumerate() {
            if after.operations.get(after_index) == Some(operation) {
                after_index += 1;
                continue;
            }
            // A narrowing may become exact; the checker proves it fits.
            if let (Operation::NarrowSize { destination, value, exact: false },
                Some(Operation::NarrowSize { destination: after_destination, value: after_value, exact: true }))
                = (operation, after.operations.get(after_index))
                && destination == after_destination && value == after_value
            {
                after_index += 1;
                changed = true;
                continue;
            }
            if !matches!(operation, Operation::Guard { kind: GuardKind::Bounds | GuardKind::Arithmetic, .. }) {
                return Err(RewriteRelationError::OperationChanged);
            }
            removed.insert((before.id, index));
        }
        if after_index != after.operations.len() {
            return Err(RewriteRelationError::OperationChanged);
        }
        changed |= before.operations.len() != after.operations.len();
    }
    // Each removed guard and folded branch is implied by the guards that
    // remain, not by itself or another removed guard.
    let reproved = if reprove_guards { removed.iter().chain(&folded).copied().collect() } else { folded };
    if !reproved.is_empty() {
        let implied = super::analysis::implied_guards(function, raw_bytes(), |point| !removed.contains(&point));
        if reproved.iter().any(|point| implied.get(point) != Some(&true)) {
            return Err(RewriteRelationError::GuardHasNoEarlierSurvivingMatch);
        }
    }
    Ok(changed)
}

#[cfg(feature = "raw-encoder-test-api")]
#[doc(hidden)]
pub mod self_checks {
    use super::*;

    /// Over a lowered module with both proven and unproven conditional
    /// branches: only a proven one folds, and only an unnamed block goes.
    pub fn folded_branches_must_be_implied(lowered: &CanonicalModule<LoweredCandidate>) {
        let candidate = remove_redundant_bounds_guards_before_check(lowered, &BTreeSet::new())
            .expect("some guard or branch is implied");
        assert!(candidate.functions.iter().zip(&lowered.functions)
            .any(|(after, before)| after.blocks.len() < before.blocks.len()),
            "a proven branch folds and its diagnostic block goes");

        let raw_bytes = super::super::analysis::raw_byte_layouts(lowered);
        let (function, block) = lowered.functions.iter().enumerate().find_map(|(index, function)| {
            let implied = super::super::analysis::redundant_bounds_guards(function, &raw_bytes);
            function.blocks.iter().position(|block| matches!(block.terminator, Terminator::BranchIf { .. })
                && implied.get(&(block.id, block.operations.len())) != Some(&true))
                .map(|block| (index, block))
        }).expect("an unproven conditional branch");
        let mut forged = lowered.clone();
        let block = &mut forged.functions[function].blocks[block];
        let Terminator::BranchIf { then_target, then_arguments, .. } = &mut block.terminator else { unreachable!() };
        let (target, arguments) = (*then_target, std::mem::take(then_arguments));
        block.terminator = Terminator::Branch { target, arguments };
        assert_eq!(check_rewrite_relation_between(lowered, &forged, false),
            Err(RewriteRelationError::GuardHasNoEarlierSurvivingMatch), "an unproven fold is refused");

        let mut dangling = lowered.clone();
        let function = dangling.functions.iter_mut().find(|function| function.blocks.len() > 1)
            .expect("a branching function");
        let named = function.blocks.iter().flat_map(|block| super::super::check::successors(&block.terminator))
            .next().expect("a branch target");
        function.blocks.retain(|block| block.id != named);
        assert_eq!(check_rewrite_relation_between(lowered, &dangling, false),
            Err(RewriteRelationError::BlockShapeChanged), "a block a kept branch names cannot go");
    }

    /// The rewrite marks some narrowing exact. The returned module instead
    /// folds, unproven, every branch that bounds a narrowed value, and marks
    /// every narrowing exact: the checker must refuse it.
    pub fn unproven_exact_narrowing(lowered: &CanonicalModule<LoweredCandidate>) -> CanonicalModule<LoweredCandidate> {
        let candidate = remove_redundant_bounds_guards_before_check(lowered, &BTreeSet::new())
            .expect("some guard or narrowing is implied");
        assert!(candidate.functions.iter().flat_map(|function| &function.blocks)
            .flat_map(|block| &block.operations)
            .any(|operation| matches!(operation, Operation::NarrowSize { exact: true, .. })),
            "a narrowing below a widened Size becomes exact");
        let mut forged = lowered.clone();
        for function in &mut forged.functions {
            let narrowed: BTreeSet<_> = function.blocks.iter().flat_map(|block| &block.operations)
                .filter_map(|operation| match operation {
                    Operation::NarrowSize { value, .. } => Some(*value),
                    _ => None,
                }).collect();
            let bounding: BTreeSet<_> = function.blocks.iter().flat_map(|block| &block.operations)
                .filter_map(|operation| match operation {
                    Operation::Compare { destination, left, .. } if narrowed.contains(left) => Some(*destination),
                    _ => None,
                }).collect();
            let traps: BTreeSet<_> = function.blocks.iter()
                .filter(|block| block.operations.is_empty() && matches!(block.terminator, Terminator::Trap))
                .map(|block| block.id).collect();
            for block in &mut function.blocks {
                for operation in &mut block.operations {
                    if let Operation::NarrowSize { exact, .. } = operation { *exact = true; }
                }
                if let Terminator::BranchIf { condition, then_target, then_arguments, else_target, .. } = &mut block.terminator
                    && bounding.contains(condition) && traps.contains(else_target)
                {
                    let (target, arguments) = (*then_target, std::mem::take(then_arguments));
                    block.terminator = Terminator::Branch { target, arguments };
                }
            }
            let reachable = reachable_blocks(function);
            function.blocks.retain(|block| reachable.contains(&block.id));
        }
        let referenced = referenced_static_data(&forged);
        forged.static_data_segments.retain(|segment| referenced.contains(&segment.id()));
        forged
    }
}
