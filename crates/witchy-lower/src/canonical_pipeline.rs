//! Internal RFC-0165 stage composition.
//!
//! This is intentionally crate-private until its inputs come directly from a
//! `CheckedModule` source witness. It produces final SafeWIR authority, never
//! bytes, and has no raw-WIR fallback.

use std::fmt;

use witchy_types::typeck::TypedModule;
use witchy_wir::canonical_safewir::{
    CheckedSourceIdentity, FinalValidationError, InitialCheckError, OptimizationError,
    ValidatedSafeWir, check_initial_with_witness, optimize_unreachable_blocks, validate_final,
};
use witchy_wir::layout::LayoutBundle;

use crate::canonical_lower::{CanonicalLoweringError, lower_typed_module};

#[derive(Debug)]
pub(crate) enum CanonicalPipelineError {
    Lowering(CanonicalLoweringError),
    InitialValidation(InitialCheckError),
    Optimization(OptimizationError),
    FinalValidation(FinalValidationError),
}

impl fmt::Display for CanonicalPipelineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "canonical SafeWIR pipeline failed: {self:?}")
    }
}

impl std::error::Error for CanonicalPipelineError {}

pub(crate) fn validate_typed_module(
    typed: &TypedModule,
    checked_source: CheckedSourceIdentity,
    layouts: LayoutBundle,
) -> Result<ValidatedSafeWir, CanonicalPipelineError> {
    let candidate = lower_typed_module(typed, checked_source, layouts)
        .map_err(CanonicalPipelineError::Lowering)?;
    let checked = check_initial_with_witness(candidate)
        .map_err(CanonicalPipelineError::InitialValidation)?;
    let initial_snapshot = checked.module().clone();
    let (initial, lowering_witness) = checked.into_parts();
    let optimized = optimize_unreachable_blocks(initial)
        .map_err(CanonicalPipelineError::Optimization)?;
    validate_final(&initial_snapshot, lowering_witness, optimized)
        .map_err(CanonicalPipelineError::FinalValidation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchy_syntax::parser::parse_module;
    use witchy_wir::layout::{LayoutInterner, LayoutBundle};

    #[test]
    fn scalar_source_reaches_final_safewir_authority_without_raw_wir() {
        let source = parse_module("fn main() -> Int:\n    40 + 2\n").expect("parse fixture");
        let typed = witchy_types::typeck::annotate_checked(source).expect("type check fixture");
        let layouts = LayoutBundle::from_interner(&LayoutInterner::new(), [])
            .expect("empty scalar layout bundle");
        let validated = validate_typed_module(
            &typed,
            CheckedSourceIdentity::from_digest([9; 32]),
            layouts,
        )
        .expect("validate canonical scalar pipeline");

        assert_eq!(validated.checked_source(), CheckedSourceIdentity::from_digest([9; 32]));
        assert!(validated.optimization_history().is_zero_pass());
    }
}
