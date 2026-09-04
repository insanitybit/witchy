//! Internal RFC-0165 stage composition.
//!
//! This is intentionally crate-private until its inputs come directly from a
//! `CheckedModule` canonical source product. It produces final SafeWIR authority, never
//! bytes, and has no raw-WIR fallback.

use std::fmt;

use witchy_types::pipeline::CheckedModule;
use witchy_wir::canonical_safewir::{
    FinalValidationError, InitialCheckError, OptimizationError,
    ValidatedSafeWir, check_initial_with_witness, optimize_unreachable_blocks, validate_final,
};
use witchy_wir::layout::LayoutBundle;

use crate::canonical_lower::{CanonicalLoweringError, lower_typed_module};

#[derive(Debug)]
pub(crate) enum CanonicalPipelineError {
    MissingCheckedSourceWitness,
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

pub(crate) fn validate_checked_module(
    checked: &CheckedModule,
    layouts: LayoutBundle,
) -> Result<ValidatedSafeWir, CanonicalPipelineError> {
    let source = checked
        .canonical_source()
        .ok_or(CanonicalPipelineError::MissingCheckedSourceWitness)?;
    let candidate = lower_typed_module(source, layouts)
        .map_err(CanonicalPipelineError::Lowering)?;
    let checked = check_initial_with_witness(
        candidate,
        source.source_witness().proper_tail_manifest_digest(),
    )
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
        let source = "fn main() -> Int:\n    40 + 2\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked source");
        let layouts = LayoutBundle::from_interner(&LayoutInterner::new(), [])
            .expect("empty scalar layout bundle");
        let validated = validate_checked_module(&checked, layouts)
        .expect("validate canonical scalar pipeline");

        assert_eq!(validated.checked_source().digest(), checked.canonical_source_witness().unwrap().source_bundle_digest());
        assert!(validated.optimization_history().is_zero_pass());
    }

    #[test]
    fn ast_only_checked_source_fails_closed_before_canonical_lowering() {
        let source = parse_module("fn main() -> Int:\n    42\n").expect("parse fixture");
        let checked = witchy_types::pipeline::link_checked(
            vec![("main".to_string(), source)],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked AST source");
        let layouts = LayoutBundle::from_interner(&LayoutInterner::new(), [])
            .expect("empty scalar layout bundle");

        assert!(matches!(
            validate_checked_module(&checked, layouts),
            Err(CanonicalPipelineError::MissingCheckedSourceWitness)
        ));
    }
}
