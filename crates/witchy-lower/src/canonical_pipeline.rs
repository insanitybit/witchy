//! Internal RFC-0165 stage composition.
//!
//! This is intentionally crate-private until its inputs come directly from a
//! `CheckedModule` canonical source product. It produces final SafeWIR authority, never
//! bytes, and has no raw-WIR fallback.

use std::fmt;

use witchy_types::pipeline::CheckedModule;
use witchy_wir::canonical_safewir::{
    FinalValidationError, InitialCheckError, OptimizationError, OptimizationSession,
    ValidatedSafeWir, check_initial_with_witness, validate_final,
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
    let mut optimizer = OptimizationSession::new(initial);
    optimizer
        .apply_unreachable_blocks()
        .map_err(CanonicalPipelineError::Optimization)?;
    optimizer
        .apply_constant_folding()
        .map_err(CanonicalPipelineError::Optimization)?;
    let optimized = optimizer
        .finalize()
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
        let _history = validated.optimization_history();
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
    fn checked_scalar_direct_call_reaches_admitted_execution() {
        let source = "fn answer() -> Int:\n    42\nfn main() -> Int:\n    answer()\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked direct-call source");

        let artifact = admit_checked_module(&checked)
            .expect("admit canonical direct-call artifact");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted direct-call artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
            .expect("instantiate admitted direct-call artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f1")
            .expect("canonical main export");

        assert_eq!(main.call(&mut store, ()).expect("run canonical main"), 42);
    }

    #[test]
    fn checked_parameterized_call_reaches_admitted_execution() {
        let source = "fn increment(n: Int) -> Int:\n    n + 1\nfn main() -> Int:\n    increment(41)\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked parameterized-call source");

        let artifact = admit_checked_module(&checked)
            .expect("admit canonical parameterized-call artifact");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted parameterized-call artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
            .expect("instantiate admitted parameterized-call artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f1")
            .expect("canonical main export");

        assert_eq!(main.call(&mut store, ()).expect("run canonical main"), 42);
    }

    #[test]
    fn checked_proper_tail_call_reaches_return_call_execution() {
        let source = "fn answer() -> Int:\n    42\nfn main() -> Int:\n    return answer()\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked proper-tail source");

        let artifact = admit_checked_module(&checked)
            .expect("admit canonical proper-tail artifact");
        assert!(artifact.wasm().windows(2).any(|bytes| bytes == [0x12, 0x00]));
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted proper-tail artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
            .expect("instantiate admitted proper-tail artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f1")
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
    fn normal_mode_list_push_reallocates_and_preserves_mutation_syntax() {
        let source = "fn main() -> Int:\n    var xs = [1]\n    xs.push(2)\n    xs[1]\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked List(Int) push source");

        let artifact = admit_checked_module(&checked)
            .expect("admit canonical List(Int) push artifact");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted List(Int) push artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
            .expect("instantiate admitted List(Int) push artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f0")
            .expect("canonical main export");

        assert_eq!(main.call(&mut store, ()).expect("run canonical List push"), 2);
    }

    #[test]
    fn list_push_keeps_length_bounds_check_for_out_of_bounds_index() {
        let source = "fn main() -> Int:\n    var xs = [1]\n    xs.push(2)\n    xs[2]\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked out-of-bounds List(Int) push source");
        let artifact = admit_checked_module(&checked)
            .expect("bounds-checked List push still admits");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile bounds-checked List push artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
            .expect("instantiate bounds-checked List push artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f0")
            .expect("canonical main export");

        assert!(main.call(&mut store, ()).is_err());
    }

    #[test]
    fn borrowed_list_parameter_index_reaches_admitted_execution() {
        let source = "fn first(xs: List(Int)) -> Int:\n    xs[0]\nfn main() -> Int:\n    first([42])\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked borrowed List(Int) source");

        let artifact = admit_checked_module(&checked)
            .expect("admit canonical borrowed List(Int) artifact");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted borrowed List(Int) artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
            .expect("instantiate admitted borrowed List(Int) artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f1")
            .expect("canonical main export");

        assert_eq!(main.call(&mut store, ()).expect("run canonical main"), 42);
    }

    #[test]
    fn list_object_survives_loop_cfg_join_and_admitted_execution() {
        let source = "fn main() -> Int:\n    let xs = [42]\n    var i = 0\n    while i < 1:\n        xs[0]\n        i = i + 1\n    xs[0]\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked list loop source");

        let artifact = admit_checked_module(&checked)
            .expect("admit canonical list loop artifact");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted list loop artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
            .expect("instantiate admitted list loop artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f0")
            .expect("canonical main export");

        assert_eq!(main.call(&mut store, ()).expect("run canonical list loop"), 42);
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
    fn checked_string_signature_and_utf8_literal_project_one_deterministic_byte_layout() {
        let source = "fn text() -> String:\n    \"h\u{00e9}llo\"\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked String source");

        let canonical = checked.canonical_source().expect("canonical source");
        let first = derive_layout_bundle(canonical).expect("project canonical String layout");
        let second = derive_layout_bundle(canonical).expect("reproject canonical String layout");
        assert_eq!(first.canonical_bytes(), second.canonical_bytes());
        assert_eq!(first.roots().len(), 1);

        let (_, layouts) = witchy_wir::layout::LayoutBundle::decode_canonical(&first.canonical_bytes())
            .expect("decode projected String layout bundle");
        let string = layouts.get(first.roots()[0]).expect("String descriptor");
        let element = match string.kind() {
            witchy_wir::layout::LayoutKind::RuntimeBuffer { element } => *element,
            other => panic!("expected canonical String byte buffer, got {other:?}"),
        };
        assert_eq!(
            string.size(),
            witchy_wir::layout::LayoutSize::Dynamic { base: 4, stride: 1 }
        );
        assert_eq!(string.header(), witchy_wir::layout::HeaderLayout::None);
        assert_eq!(
            layouts.get(element).expect("RawByte descriptor").kind(),
            &witchy_wir::layout::LayoutKind::Scalar(witchy_wir::layout::ScalarKind::RawByte)
        );
    }

    #[test]
    fn checked_utf8_string_literal_reaches_admitted_static_data_pointer() {
        let source = "fn text() -> String:\n    \"h\u{00e9}llo\"\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("text".to_string(), source.as_bytes().to_vec())],
            "text",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked UTF-8 String source");

        let canonical = checked.canonical_source().expect("canonical source");
        let layouts = derive_layout_bundle(canonical).expect("derive String layout");
        let candidate = lower_typed_module(canonical, layouts)
            .expect("lower canonical UTF-8 String literal");
        let segments = candidate.static_data_segments();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].id(), witchy_wir::canonical_safewir::StaticDataId::new(0));
        assert_eq!(segments[0].bytes(), &[6, 0, 0, 0, b'h', 0xc3, 0xa9, b'l', b'l', b'o']);

        let artifact = admit_checked_module(&checked).expect("admit UTF-8 String literal");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted UTF-8 String literal");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
            .expect("instantiate admitted UTF-8 String literal");
        let main = instance
            .get_typed_func::<(), i32>(&mut store, "f0")
            .expect("String result export");
        assert_eq!(main.call(&mut store, ()).expect("run String literal"), 16);
    }

    #[test]
    fn repeated_string_literals_share_one_deterministic_static_segment() {
        let source = "fn main() -> Int:\n    let first = \"same\"\n    let second = \"same\"\n    1\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked repeated String source");
        let canonical = checked.canonical_source().expect("canonical source");
        let layouts = derive_layout_bundle(canonical).expect("derive String layout");
        let candidate = lower_typed_module(canonical, layouts)
            .expect("lower repeated String literals");
        assert_eq!(candidate.static_data_segments().len(), 1);
        assert_eq!(candidate.static_data_segments()[0].id(), witchy_wir::canonical_safewir::StaticDataId::new(0));
        assert_eq!(candidate.static_data_segments()[0].bytes(), &[4, 0, 0, 0, b's', b'a', b'm', b'e']);
    }
}
