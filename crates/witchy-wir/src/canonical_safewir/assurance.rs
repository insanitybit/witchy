use super::history::{
    module_digest, tail_digest, DischargedObligation, OptimizationPassId, RefinementReceipt,
};
use super::{
    check_transform, CanonicalModule, InitialCheckError, ModuleStage, OptimizedValid,
    TransformedCandidate,
};

/// The semantic family a production optimization belongs to. This is part of
/// the policy identity, so changing a pass cannot silently preserve old
/// optimization history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptimizationPassClass {
    ControlFlow,
    ScalarEvaluation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafetyRecheck {
    CanonicalSafeWir,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefinementMechanism {
    ExactRetainedBlockCorrespondence,
    ExecutableScalarReferenceSemantics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PassAssurance {
    pass: OptimizationPassId,
    class: OptimizationPassClass,
    safety_recheck: Option<SafetyRecheck>,
    refinement: Option<RefinementMechanism>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssuranceError {
    UnregisteredPass(OptimizationPassId),
    IncompleteRecord(OptimizationPassId),
    ReceiptNotBound(OptimizationPassId),
    RefinementMechanismMismatch(OptimizationPassId),
    Safety(InitialCheckError),
}

impl PassAssurance {
    const fn complete(
        pass: OptimizationPassId,
        class: OptimizationPassClass,
        refinement: RefinementMechanism,
    ) -> Self {
        Self {
            pass,
            class,
            safety_recheck: Some(SafetyRecheck::CanonicalSafeWir),
            refinement: Some(refinement),
        }
    }

    /// The fallible constructor keeps incomplete records representable in
    /// policy tests, but rejects them before they can be used.
    pub fn try_from_parts(
        pass: OptimizationPassId,
        class: OptimizationPassClass,
        safety_recheck: Option<SafetyRecheck>,
        refinement: Option<RefinementMechanism>,
    ) -> Result<Self, AssuranceError> {
        let record = Self { pass, class, safety_recheck, refinement };
        record.validate()?;
        Ok(record)
    }

    fn validate(&self) -> Result<(), AssuranceError> {
        if self.safety_recheck.is_none() || self.refinement.is_none() {
            return Err(AssuranceError::IncompleteRecord(self.pass));
        }
        Ok(())
    }

    pub const fn pass(self) -> OptimizationPassId {
        self.pass
    }

    pub const fn class(self) -> OptimizationPassClass {
        self.class
    }

    pub const fn safety_recheck(self) -> Option<SafetyRecheck> {
        self.safety_recheck
    }

    pub const fn refinement(self) -> Option<RefinementMechanism> {
        self.refinement
    }

    fn accepts_receipt(self, receipt: &RefinementReceipt) -> bool {
        match (self.refinement, receipt.pass()) {
            (
                Some(RefinementMechanism::ExactRetainedBlockCorrespondence),
                OptimizationPassId::UnreachableBlockPruning,
            ) => {
                !receipt.discharged_obligations().is_empty()
                    && receipt.discharged_obligations().iter().all(|obligation| {
                        matches!(obligation, DischargedObligation::PrunedBlocks { .. })
                    })
            }
            (
                Some(RefinementMechanism::ExecutableScalarReferenceSemantics),
                OptimizationPassId::ConstantFolding,
            ) => {
                receipt.discharged_obligations().len() == 1
                    && matches!(
                        receipt.discharged_obligations().first(),
                        Some(DischargedObligation::FoldedScalarOperation { .. })
                    )
            }
            _ => false,
        }
    }
}

static PRODUCTION_REGISTRY: &[PassAssurance] = &[
    PassAssurance::complete(
        OptimizationPassId::UnreachableBlockPruning,
        OptimizationPassClass::ControlFlow,
        RefinementMechanism::ExactRetainedBlockCorrespondence,
    ),
    PassAssurance::complete(
        OptimizationPassId::ConstantFolding,
        OptimizationPassClass::ScalarEvaluation,
        RefinementMechanism::ExecutableScalarReferenceSemantics,
    ),
];

pub fn production_registry() -> &'static [PassAssurance] {
    PRODUCTION_REGISTRY
}

const fn policy_for(pass: OptimizationPassId) -> PassAssurance {
    match pass {
        OptimizationPassId::UnreachableBlockPruning => PassAssurance::complete(
            pass,
            OptimizationPassClass::ControlFlow,
            RefinementMechanism::ExactRetainedBlockCorrespondence,
        ),
        OptimizationPassId::ConstantFolding => PassAssurance::complete(
            pass,
            OptimizationPassClass::ScalarEvaluation,
            RefinementMechanism::ExecutableScalarReferenceSemantics,
        ),
    }
}

pub(crate) fn registered(pass: OptimizationPassId) -> Result<&'static PassAssurance, AssuranceError> {
    let expected = policy_for(pass);
    let record = PRODUCTION_REGISTRY
        .iter()
        .find(|record| record.pass() == pass && **record == expected)
        .ok_or(AssuranceError::UnregisteredPass(pass))?;
    record.validate().map(|()| record)
}

pub(crate) fn receipt_matches_registered(receipt: &RefinementReceipt) -> bool {
    registered(receipt.pass())
        .map(|record| record.accepts_receipt(receipt))
        .unwrap_or(false)
}

pub(crate) fn recheck(
    pass: OptimizationPassId,
    candidate: CanonicalModule<TransformedCandidate>,
) -> Result<CanonicalModule<OptimizedValid>, AssuranceError> {
    let record = registered(pass)?;
    match record.safety_recheck() {
        Some(SafetyRecheck::CanonicalSafeWir) => {
            check_transform(candidate).map_err(AssuranceError::Safety)
        }
        None => Err(AssuranceError::IncompleteRecord(pass)),
    }
}

pub(crate) fn accept_transform<S: ModuleStage>(
    pass: OptimizationPassId,
    before: &CanonicalModule<S>,
    candidate: CanonicalModule<TransformedCandidate>,
    receipt: RefinementReceipt,
) -> Result<(CanonicalModule<OptimizedValid>, RefinementReceipt), AssuranceError> {
    let record = registered(pass)?;
    if receipt.pass() != pass
        || receipt.before_digest() != module_digest(before)
        || receipt.after_digest() != module_digest(&candidate)
        || receipt.before_revision() != before.revision()
        || receipt.after_revision() != candidate.revision()
        || receipt.before_tail_digest() != tail_digest(before)
        || receipt.after_tail_digest() != tail_digest(&candidate)
    {
        return Err(AssuranceError::ReceiptNotBound(pass));
    }
    if !record.accepts_receipt(&receipt) {
        return Err(AssuranceError::RefinementMechanismMismatch(pass));
    }
    let optimized = recheck(pass, candidate)?;
    Ok((optimized, receipt))
}

pub(crate) fn policy_digest_bytes() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(PRODUCTION_REGISTRY.len() * 4);
    for record in PRODUCTION_REGISTRY {
        bytes.push(pass_tag(record.pass()));
        bytes.push(class_tag(record.class()));
        bytes.push(safety_tag(record.safety_recheck()));
        bytes.push(refinement_tag(record.refinement()));
    }
    bytes
}

const fn pass_tag(pass: OptimizationPassId) -> u8 {
    match pass {
        OptimizationPassId::UnreachableBlockPruning => 0,
        OptimizationPassId::ConstantFolding => 1,
    }
}

const fn class_tag(class: OptimizationPassClass) -> u8 {
    match class {
        OptimizationPassClass::ControlFlow => 0,
        OptimizationPassClass::ScalarEvaluation => 1,
    }
}

const fn safety_tag(safety_recheck: Option<SafetyRecheck>) -> u8 {
    match safety_recheck {
        Some(SafetyRecheck::CanonicalSafeWir) => 0,
        None => 255,
    }
}

const fn refinement_tag(refinement: Option<RefinementMechanism>) -> u8 {
    match refinement {
        Some(RefinementMechanism::ExactRetainedBlockCorrespondence) => 0,
        Some(RefinementMechanism::ExecutableScalarReferenceSemantics) => 1,
        None => 255,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_registry_is_exhaustive_for_every_pass_id() {
        assert_eq!(production_registry().len(), OptimizationPassId::ALL.len());
        for pass in OptimizationPassId::ALL {
            let record = registered(pass).expect("every pass is registered");
            assert_eq!(record.pass(), pass);
            assert!(record.safety_recheck().is_some());
            assert!(record.refinement().is_some());
        }
    }

    #[test]
    fn incomplete_assurance_records_are_rejected() {
        let missing_safety = PassAssurance::try_from_parts(
            OptimizationPassId::ConstantFolding,
            OptimizationPassClass::ScalarEvaluation,
            None,
            Some(RefinementMechanism::ExecutableScalarReferenceSemantics),
        );
        assert!(matches!(missing_safety, Err(AssuranceError::IncompleteRecord(_))));

        let missing_refinement = PassAssurance::try_from_parts(
            OptimizationPassId::ConstantFolding,
            OptimizationPassClass::ScalarEvaluation,
            Some(SafetyRecheck::CanonicalSafeWir),
            None,
        );
        assert!(matches!(missing_refinement, Err(AssuranceError::IncompleteRecord(_))));
    }
}
