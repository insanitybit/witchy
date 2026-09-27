//! Internal RFC-0165 stage composition.
//!
//! This is intentionally crate-private until its inputs come directly from a
//! `CheckedModule` canonical source product. It produces final SafeWIR authority, never
//! bytes, and has no raw-WIR fallback.

use std::fmt;

use witchy_types::pipeline::{CanonicalCheckedSource, CheckedModule};
use witchy_wir::canonical_safewir::optimize::{
    PassId, run_checked_pass,
};
use witchy_wir::canonical_safewir::{
    FinalValidationError, InitialWitnessError, ValidatedSafeWir, check_initial_with_witness,
    validate_final,
};
use witchy_wir::{AdmittedCanonicalArtifact, CanonicalAdmissionError};

use crate::canonical_layouts::{CanonicalLayoutError, derive_layout_bundle};
use crate::canonical_lower::{CanonicalLoweringError, lower_typed_module};

#[derive(Debug)]
pub(crate) enum CanonicalPipelineError {
    MissingCheckedSourceWitness,
    Layout(CanonicalLayoutError),
    Lowering(CanonicalLoweringError),
    InitialValidation(InitialWitnessError),
    FinalValidation(FinalValidationError),
    Admission(CanonicalAdmissionError),
}

pub(crate) fn admit_checked_module(
    checked: &CheckedModule,
) -> Result<AdmittedCanonicalArtifact, CanonicalPipelineError> {
    admit_checked_module_with_observer(checked, |_, _| ())
        .map(|(artifact, ())| artifact)
}

pub(crate) fn admit_checked_module_with_observer<R>(
    checked: &CheckedModule,
    observe: impl FnOnce(&ValidatedSafeWir, &AdmittedCanonicalArtifact) -> R,
) -> Result<(AdmittedCanonicalArtifact, R), CanonicalPipelineError> {
    let source = checked
        .canonical_source()
        .ok_or(CanonicalPipelineError::MissingCheckedSourceWitness)?;
    admit_checked_source_with_observer(source, observe)
}

pub(crate) fn admit_checked_source(
    source: &CanonicalCheckedSource,
) -> Result<AdmittedCanonicalArtifact, CanonicalPipelineError> {
    admit_checked_source_with_observer(source, |_, _| ()).map(|(artifact, ())| artifact)
}

pub(crate) fn admit_unoptimized_checked_source(
    source: &CanonicalCheckedSource,
) -> Result<AdmittedCanonicalArtifact, CanonicalPipelineError> {
    let validated = validate_unoptimized_checked_source(source)?;
    witchy_wir::admit_canonical_with_observer(validated, &source.view(), |_, _| ())
        .map(|(artifact, ())| artifact).map_err(CanonicalPipelineError::Admission)
}

fn admit_checked_source_with_observer<R>(
    source: &CanonicalCheckedSource,
    observe: impl FnOnce(&ValidatedSafeWir, &AdmittedCanonicalArtifact) -> R,
) -> Result<(AdmittedCanonicalArtifact, R), CanonicalPipelineError> {
    // The observer is invoked only after an artifact is admitted, so a failed
    // optional transformation cannot consume it before the baseline retry.
    let mut observe = Some(observe);
    witchy_canonical_source::aggregates::with_shape_memo(source.type_definitions(), || {
        let validated = validate_checked_source(source)?;
        let first = witchy_wir::admit_canonical_with_observer(validated, &source.view(), |wir, artifact| {
            observe.take().expect("admitted artifact is observed once")(wir, artifact)
        });
        match first {
            Ok(admitted) => Ok(admitted),
            Err(_) if use_bounds_guard_pass(source) => {
                // A transformation has no authority to make the baseline
                // unexecutable. Retry from checked source through every
                // checker and Wasm admission stage, never from its output.
                let baseline = validate_unoptimized_checked_source(source)?;
                witchy_wir::admit_canonical_with_observer(baseline, &source.view(), |wir, artifact| {
                    observe.take().expect("admitted artifact is observed once")(wir, artifact)
                }).map_err(CanonicalPipelineError::Admission)
            }
            Err(error) => Err(CanonicalPipelineError::Admission(error)),
        }
    })
}

fn source_uses_opt(source: &CanonicalCheckedSource) -> bool {
    source.typed_module().module().modes.iter().any(|mode| mode == "opt")
}

fn use_bounds_guard_pass(source: &CanonicalCheckedSource) -> bool {
    source_uses_opt(source)
        && witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::BoundsElide)
}

fn validate_unoptimized_checked_source(
    source: &CanonicalCheckedSource,
) -> Result<ValidatedSafeWir, CanonicalPipelineError> {
    crate::canonical_layouts::with_layout_memo(source, || {
        let layouts = derive_layout_bundle(source).map_err(CanonicalPipelineError::Layout)?;
        let candidate = lower_typed_module(source, layouts)
            .map_err(CanonicalPipelineError::Lowering)?
            .with_launch_contract_payload(source.launch_contract_payload().to_vec());
        let checked = check_initial_with_witness(candidate, &source.view())
            .map_err(CanonicalPipelineError::InitialValidation)?;
        validate_final(checked, &source.view()).map_err(CanonicalPipelineError::FinalValidation)
    })
}

impl fmt::Display for CanonicalPipelineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "canonical SafeWIR pipeline failed: {self:?}")
    }
}

impl std::error::Error for CanonicalPipelineError {}

pub(crate) fn validate_checked_module(
    checked: &CheckedModule,
) -> Result<ValidatedSafeWir, CanonicalPipelineError> {
    let source = checked
        .canonical_source()
        .ok_or(CanonicalPipelineError::MissingCheckedSourceWitness)?;
    validate_checked_source(source)
}

pub(crate) fn validate_checked_source(
    source: &CanonicalCheckedSource,
) -> Result<ValidatedSafeWir, CanonicalPipelineError> {
    // Layout derivation and lowering query the same source's projection five
    // times; share one (see `canonical_layouts::with_layout_memo`).
    crate::canonical_layouts::with_layout_memo(source, || {
        witchy_canonical_source::aggregates::with_shape_memo(source.type_definitions(), || {
            validate_checked_source_inner(source)
        })
    })
}

fn validate_checked_source_inner(
    source: &CanonicalCheckedSource,
) -> Result<ValidatedSafeWir, CanonicalPipelineError> {
    let layouts = derive_layout_bundle(source).map_err(CanonicalPipelineError::Layout)?;
    let candidate = lower_typed_module(source, layouts)
        .map_err(CanonicalPipelineError::Lowering)?
        .with_launch_contract_payload(source.launch_contract_payload().to_vec());
    let baseline_candidate = use_bounds_guard_pass(source).then(|| candidate.clone());
    let checked = check_initial_with_witness(candidate, &source.view())
        .map_err(CanonicalPipelineError::InitialValidation)?;
    if let Some(baseline_candidate) = baseline_candidate {
        match run_checked_pass(checked, PassId::SameBlockBoundsGuards).validate_final(&source.view()) {
            Ok(validated) => Ok(validated),
            Err(_) => {
                let baseline = check_initial_with_witness(baseline_candidate, &source.view())
                    .map_err(CanonicalPipelineError::InitialValidation)?;
                validate_final(baseline, &source.view()).map_err(CanonicalPipelineError::FinalValidation)
            }
        }
    } else {
        // Ordinary mode remains the zero-pass baseline. The final validator
        // consumes and rechecks this exact sealed graph; functions whose
        // checker inputs are byte-identical to the initial pass replay their
        // admitted verdict from the per-function memo (`check::cache`).
        validate_final(checked, &source.view()).map_err(CanonicalPipelineError::FinalValidation)
    }
}
