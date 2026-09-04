//! Internal RFC-0165 stage composition.
//!
//! This is intentionally crate-private until its inputs come directly from a
//! `CheckedModule` canonical source product. It produces final SafeWIR authority, never
//! bytes, and has no raw-WIR fallback.

use std::fmt;

use witchy_types::pipeline::CheckedModule;
use witchy_wir::canonical_safewir::{
    FinalValidationError, InitialCheckError, OptimizationError, ValidatedSafeWir,
    check_initial_with_witness, optimize_unreachable_blocks, validate_final,
};
use witchy_wir::{AdmittedCanonicalArtifact, CanonicalAdmissionError, admit_canonical};

use crate::canonical_layouts::{CanonicalLayoutError, derive_layout_bundle};
use crate::canonical_lower::{CanonicalLoweringError, lower_typed_module};

#[derive(Debug)]
pub(crate) enum CanonicalPipelineError {
    MissingCheckedSourceWitness,
    Layout(CanonicalLayoutError),
    Lowering(CanonicalLoweringError),
    InitialValidation(InitialCheckError),
    Optimization(OptimizationError),
    FinalValidation(FinalValidationError),
    Admission(CanonicalAdmissionError),
}

pub(crate) fn admit_checked_module(
    checked: &CheckedModule,
) -> Result<AdmittedCanonicalArtifact, CanonicalPipelineError> {
    let validated = validate_checked_module(checked)?;
    admit_canonical(validated).map_err(CanonicalPipelineError::Admission)
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
    let layouts = derive_layout_bundle(source).map_err(CanonicalPipelineError::Layout)?;
    let candidate = lower_typed_module(source, layouts)
        .map_err(CanonicalPipelineError::Lowering)?;
    let checked = check_initial_with_witness(candidate, source)
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

    #[test]
    fn scalar_source_reaches_final_safewir_authority_without_raw_wir() {
        let source = "fn main() -> Int:\n    40 + 2\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked source");
        let validated = validate_checked_module(&checked)
        .expect("validate canonical scalar pipeline");

        assert_eq!(validated.checked_source().digest(), checked.canonical_source_witness().unwrap().source_bundle_digest());
        assert!(validated.optimization_history().is_zero_pass());
    }

    #[test]
    fn scalar_checked_source_reaches_exact_canonical_artifact() {
        let source = "fn main() -> Int:\n    40 + 2\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked source");
        let artifact = admit_checked_module(&checked)
            .expect("admit canonical scalar artifact");

        wasmparser::validate(artifact.wasm()).expect("admitted bytes remain valid Wasm");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted canonical artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
            .expect("instantiate admitted canonical artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f0")
            .expect("canonical main export");
        assert_eq!(main.call(&mut store, ()).expect("run canonical main"), 42);
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
        assert!(matches!(
            validate_checked_module(&checked),
            Err(CanonicalPipelineError::MissingCheckedSourceWitness)
        ));
    }

    #[test]
    fn proper_tail_mapping_ignores_prefix_items_and_noncanonical_functions() {
        let source = "type Marker:\n    Value\n\nfn helper() -> Int:\n    7\n\nfn recurse() -> Int:\n    recurse()\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked source with prefix items");
        validate_checked_module(&checked)
            .expect("proper-tail source mapping should use canonical function ordinals");
    }

    #[test]
    fn list_int_layout_is_projected_from_checked_types() {
        let source = "fn read(i: Int) -> Int:\n    let xs = [10, 20]\n    xs[i]\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked List(Int) source");
        let canonical = checked.canonical_source().expect("canonical source");
        let bundle = derive_layout_bundle(canonical).expect("project canonical layouts");

        assert_eq!(bundle.roots().len(), 1);
        let (_, layouts) = witchy_wir::layout::LayoutBundle::decode_canonical(&bundle.canonical_bytes())
            .expect("decode projected layout bundle");
        let descriptor = layouts.get(bundle.roots()[0]).expect("List(Int) descriptor");
        assert!(matches!(
            descriptor.kind(),
            witchy_wir::layout::LayoutKind::PackedList { .. }
        ));
        validate_checked_module(&checked).expect("validate projected List(Int) source");
    }

    #[test]
    fn list_int_source_reaches_admitted_executable_artifact() {
        let source = "fn main() -> Int:\n    [10, 20][1]\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked List(Int) source");

        let artifact = admit_checked_module(&checked)
            .expect("admit canonical List(Int) artifact");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted List(Int) artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
            .expect("instantiate admitted List(Int) artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f0")
            .expect("canonical main export");

        assert_eq!(main.call(&mut store, ()).expect("run canonical main"), 20);
    }

    #[test]
    fn list_int_out_of_bounds_source_admits_then_traps() {
        let source = "fn main() -> Int:\n    [10][1]\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked out-of-bounds source");

        let artifact = admit_checked_module(&checked)
            .expect("bounds checks permit safe canonical admission");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted bounds-checked artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
            .expect("instantiate admitted bounds-checked artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f0")
            .expect("canonical main export");

        assert!(main.call(&mut store, ()).is_err());
    }

    #[test]
    fn unsupported_string_layout_fails_closed_before_lowering() {
        let source = "fn text() -> String:\n    \"hello\"\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked String source");

        let error = match validate_checked_module(&checked) {
            Ok(_) => panic!("String is outside canonical v1"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            CanonicalPipelineError::Layout(CanonicalLayoutError::UnsupportedType(_))
        ));
    }
}
