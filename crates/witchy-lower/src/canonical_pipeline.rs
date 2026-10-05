//! Internal RFC-0165 stage composition.
//!
//! This is intentionally crate-private until its inputs come directly from a
//! `CheckedModule` canonical source product. It produces final SafeWIR authority, never
//! bytes, and has no raw-WIR fallback.

use std::fmt;

use witchy_types::pipeline::{CanonicalCheckedSource, CheckedModule};
use witchy_wir::canonical_safewir::optimize::{
    PassId, remove_redundant_bounds_guards_before_check, run_checked_pass,
};
use witchy_wir::canonical_safewir::{
    CanonicalModule, FinalValidationError, InitialWitnessError, LoweredCandidate, ValidatedSafeWir,
    check_initial_with_witness, validate_final,
};
use witchy_wir::{AdmittedCanonicalArtifact, CanonicalAdmissionError};

use crate::canonical_layouts::{CanonicalLayoutError, derive_layout_bundle};
use crate::canonical_lower::{CanonicalLoweringError, lower_typed_module, lower_typed_module_with_borrowed_views};

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
            Err(_) if use_bounds_guard_pass(source) || !source_uses_opt(source) => {
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
    !source_uses_opt(source)
        || witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::BoundsElide)
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

/// The bounds-guard pass's output, checked as the initial module instead of
/// after a check of the guarded one. The checker is the authority either way:
/// this module passes the same complete initial check and final validation.
/// `None` (nothing to remove, or a rejection) leaves the caller's sequence,
/// which then checks the guarded module exactly as before.
fn validate_guard_free_first(
    candidate: &CanonicalModule<LoweredCandidate>,
    source: &CanonicalCheckedSource,
) -> Option<ValidatedSafeWir> {
    let optimized = remove_redundant_bounds_guards_before_check(candidate)?;
    let validated = check_initial_with_witness(optimized, &source.view())
        .map_err(|error| format!("{error:?}"))
        .and_then(|checked| validate_final(checked, &source.view()).map_err(|error| format!("{error:?}")));
    if let Err(error) = &validated {
        if std::env::var_os("WIRDIAG").is_some() { eprintln!("WIRBAIL guard-free-first: {error}"); }
    }
    validated.ok()
}

fn validate_checked_source_inner(
    source: &CanonicalCheckedSource,
) -> Result<ValidatedSafeWir, CanonicalPipelineError> {
    let layouts = derive_layout_bundle(source).map_err(CanonicalPipelineError::Layout)?;
    // Borrowed view calls are an optional whole-module ABI choice. Keep the
    // ordinary owned-call graph as the checked fallback when a proper-tail or
    // another ownership obligation needs token transfer.
    if !source_uses_opt(source) {
        let borrowed_attempt = lower_typed_module_with_borrowed_views(source, layouts.clone(), true);
        if let Err(error) = &borrowed_attempt {
            if std::env::var_os("WIRDIAG").is_some() { eprintln!("WIRBAIL borrowed-view-lowering: {error:?}"); }
        }
        if let Ok(candidate) = borrowed_attempt {
            let candidate = candidate.with_launch_contract_payload(source.launch_contract_payload().to_vec());
            if let Some(validated) = validate_guard_free_first(&candidate, source) {
                return Ok(validated);
            }
            let checked_attempt = check_initial_with_witness(candidate, &source.view());
            if let Err(error) = &checked_attempt {
                if std::env::var_os("WIRDIAG").is_some() { eprintln!("WIRBAIL borrowed-view-check: {error:?}"); }
            }
            if let Ok(checked) = checked_attempt {
                // This checked pass removes only a later identical Size guard
                // in the same block. Its independent rewrite checker and final
                // validation must both succeed before the result is admitted.
                let final_attempt = run_checked_pass(checked, PassId::SameBlockBoundsGuards)
                    .validate_final(&source.view());
                if let Err(error) = &final_attempt {
                    if std::env::var_os("WIRDIAG").is_some() { eprintln!("WIRBAIL borrowed-view-final: {error:?}"); }
                }
                if let Ok(validated) = final_attempt {
                    return Ok(validated);
                }
            }
        }
    }
    let candidate = lower_typed_module(source, layouts)
        .map_err(CanonicalPipelineError::Lowering)?
        .with_launch_contract_payload(source.launch_contract_payload().to_vec());
    if use_bounds_guard_pass(source) {
        if let Some(validated) = validate_guard_free_first(&candidate, source) {
            return Ok(validated);
        }
    }
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
        // The final validator consumes and rechecks this exact sealed graph;
        // functions whose
        // checker inputs are byte-identical to the initial pass replay their
        // admitted verdict from the per-function memo (`check::cache`).
        validate_final(checked, &source.view()).map_err(CanonicalPipelineError::FinalValidation)
    }
}
