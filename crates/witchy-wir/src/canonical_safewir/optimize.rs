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
    CanonicalModule, CheckedInitialSafeWir, CheckedOptimizedSafeWir, CheckedSafeWirRevision,
    Function, GuardKind, InitialCheckError, InitialValid, LoweredCandidate, ModuleStage,
    Operation, OptimizationAuthorityError, OptimizationCandidate, Revision,
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
    for (function, redundant) in candidate.functions.iter_mut().zip(&redundant) {
        for block in &mut function.blocks {
            let block_id = block.id;
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
    }
    check_rewrite_relation_between(lowered, &candidate, false).ok()?;
    Some(candidate)
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
        || before.static_data_segments != after.static_data_segments
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
    let raw_bytes = advances_revision.then(|| super::analysis::raw_byte_layouts(before));
    for (before, after) in before.functions.iter().zip(&after.functions) {
        changed |= check_function_relation(before, after, raw_bytes.as_ref())?;
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

/// `raw_bytes` is present when each removed guard must be re-proven implied.
fn check_function_relation(
    before: &Function,
    after: &Function,
    raw_bytes: Option<&BTreeSet<crate::layout::LayoutId>>,
) -> Result<bool, RewriteRelationError> {
    if before.id != after.id
        || before.signature != after.signature
        || before.entry != after.entry
        || before.value_types != after.value_types
        || before.blocks.len() != after.blocks.len()
    {
        return Err(RewriteRelationError::FunctionShapeChanged);
    }
    let mut changed = false;
    let mut removed = BTreeSet::new();
    let function = before;
    for (before, after) in before.blocks.iter().zip(&after.blocks) {
        if before.id != after.id
            || before.parameters != after.parameters
            || before.terminator != after.terminator
        {
            return Err(RewriteRelationError::BlockShapeChanged);
        }

        // `after` keeps every operation of `before` in order, less some
        // guards; record which.
        let mut after_index = 0;
        for (index, operation) in before.operations.iter().enumerate() {
            if after.operations.get(after_index) == Some(operation) {
                after_index += 1;
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
    // Each removed guard is implied by the guards that remain, not by itself
    // or another removed guard.
    if let Some(raw_bytes) = raw_bytes && !removed.is_empty() {
        let implied = super::analysis::implied_guards(function, raw_bytes, |point| !removed.contains(&point));
        if removed.iter().any(|point| implied.get(point) != Some(&true)) {
            return Err(RewriteRelationError::GuardHasNoEarlierSurvivingMatch);
        }
    }
    Ok(changed)
}
