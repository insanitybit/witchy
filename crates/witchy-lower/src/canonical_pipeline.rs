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
use witchy_wir::{AdmittedCanonicalArtifact, CanonicalAdmissionError, admit_canonical};

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
    // One shape memo spans validation and admission (see `with_shape_memo`).
    witchy_canonical_source::aggregates::with_shape_memo(source.type_definitions(), || {
        let validated = validate_checked_source(source)?;
        witchy_wir::admit_canonical_with_observer(validated, &source.view(), observe)
            .map_err(CanonicalPipelineError::Admission)
    })
}

pub(crate) fn admit_checked_source(
    source: &CanonicalCheckedSource,
) -> Result<AdmittedCanonicalArtifact, CanonicalPipelineError> {
    witchy_canonical_source::aggregates::with_shape_memo(source.type_definitions(), || {
        let validated = validate_checked_source(source)?;
        admit_canonical(validated, &source.view()).map_err(CanonicalPipelineError::Admission)
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
    let checked = check_initial_with_witness(candidate, &source.view())
        .map_err(CanonicalPipelineError::InitialValidation)?;
    if source
        .typed_module()
        .module()
        .modes
        .iter()
        .any(|mode| mode == "opt")
    {
        run_checked_pass(checked, PassId::SameBlockBoundsGuards).validate_final(&source.view())
            .map_err(CanonicalPipelineError::FinalValidation)
    } else {
        // Ordinary mode remains the zero-pass baseline. The final validator
        // consumes and rechecks this exact sealed graph; functions whose
        // checker inputs are byte-identical to the initial pass replay their
        // admitted verdict from the per-function memo (`check::cache`).
        validate_final(checked, &source.view()).map_err(CanonicalPipelineError::FinalValidation)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Deliberately link only the exact canonical diagnostic ABIs. Unknown
    /// imports or mismatched signatures must still fail instantiation. These
    /// imports always terminate; a no-op host would conceal failed guards.
    pub(crate) fn fixture_diagnostic_linker<T: 'static>(engine: &wasmtime::Engine) -> wasmtime::Linker<T> {
        let mut linker = wasmtime::Linker::new(engine);
        linker.func_wrap("witchy", "__witchy_abort_at",
            |mut caller: wasmtime::Caller<'_, T>, message: i32, message_len: i32,
             function: i32, function_len: i32, line: i32| -> wasmtime::Result<()> {
                let message = fixture_diagnostic_text(&mut caller, message, message_len)?;
                let function = fixture_diagnostic_text(&mut caller, function, function_len)?;
                let core = witchy_syntax::diag::DiagTemplate::Fail.render(0, 0, &message);
                wasmtime::bail!("{}", witchy_syntax::diag::runtime_error(&function, line as u32, &core));
            }).expect("bind exact canonical fail ABI");
        linker.func_wrap("witchy", "__witchy_list_bounds_at",
            |mut caller: wasmtime::Caller<'_, T>, index: i64, length: i32,
             function: i32, function_len: i32, line: i32| -> wasmtime::Result<()> {
                let function = fixture_diagnostic_text(&mut caller, function, function_len)?;
                let core = witchy_syntax::diag::DiagTemplate::ListIndexOob.render(index, i64::from(length as u32), "");
                wasmtime::bail!("{}", witchy_syntax::diag::runtime_error(&function, line as u32, &core));
            }).expect("bind exact canonical list bounds ABI");
        linker
    }

    fn fixture_diagnostic_text<T>(caller: &mut wasmtime::Caller<'_, T>, pointer: i32, length: i32) -> wasmtime::Result<String> {
        let memory = caller.get_export("memory").and_then(wasmtime::Extern::into_memory)
            .ok_or_else(|| wasmtime::Error::msg("diagnostic memory missing"))?;
        let start = pointer as u32 as usize;
        let end = start.checked_add(length as u32 as usize)
            .ok_or_else(|| wasmtime::Error::msg("diagnostic slice overflow"))?;
        let bytes = memory.data(&*caller).get(start..end)
            .ok_or_else(|| wasmtime::Error::msg("diagnostic slice out of bounds"))?;
        Ok(std::str::from_utf8(bytes)
            .map_err(|_| wasmtime::Error::msg("diagnostic slice is not UTF-8"))?.to_owned())
    }

    #[test]
    fn fixture_diagnostics_reject_wrong_imports_and_never_continue() {
        let engine = wasmtime::Engine::default();
        let linker = fixture_diagnostic_linker(&engine);
        let mut store = wasmtime::Store::new(&engine, ());
        for import in [
            "(import \"other\" \"__witchy_abort_at\" (func (param i32 i32 i32 i32 i32)))",
            "(import \"witchy\" \"__witchy_abort_at_alias\" (func (param i32 i32 i32 i32 i32)))",
            "(import \"witchy\" \"__witchy_abort_at\" (func (param i32 i32 i32 i32 i32) (result i32)))",
            "(import \"witchy\" \"__witchy_list_bounds_at\" (func (param i32 i32 i32 i32 i32)))",
        ] {
            let module = wasmtime::Module::new(&engine, format!("(module {import})")).unwrap();
            assert!(linker.instantiate(&mut store, &module).is_err(), "reject {import}");
        }
        let module = wasmtime::Module::new(&engine, r#"(module
            (import "witchy" "__witchy_abort_at" (func $abort (param i32 i32 i32 i32 i32)))
            (import "witchy" "__witchy_list_bounds_at" (func $bounds (param i64 i32 i32 i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "boommain")
            (global $continued (export "continued") (mut i32) (i32.const 0))
            (func (export "abort")
                i32.const 0 i32.const 4 i32.const 4 i32.const 4 i32.const 8 call $abort
                i32.const 1 global.set $continued)
            (func (export "bounds")
                i64.const -1 i32.const 2 i32.const 4 i32.const 4 i32.const 8 call $bounds
                i32.const 1 global.set $continued)
            (func (export "bad_slice")
                i64.const 0 i32.const 0 i32.const -1 i32.const 1 i32.const 8 call $bounds
                i32.const 1 global.set $continued))"#).unwrap();
        let instance = linker.instantiate(&mut store, &module).unwrap();
        for (name, expected) in [
            ("abort", "runtime error: `main`, line 8: boom"),
            ("bounds", "runtime error: `main`, line 8: list index -1 out of bounds (length 2)"),
            ("bad_slice", "diagnostic slice out of bounds"),
        ] {
            let call = instance.get_typed_func::<(), ()>(&mut store, name).unwrap();
            assert_eq!(call.call(&mut store, ()).unwrap_err().root_cause().to_string(), expected);
            assert_eq!(instance.get_global(&mut store, "continued").unwrap().get(&mut store).i32(), Some(0));
        }
    }

    use witchy_syntax::parser::parse_module;

    #[test]
    fn admitted_artifact_carries_checked_launch_contract_and_refuses_mutation() {
        let source = "fn main(console: Console):\n    console.print(\"ok\")\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked source");
        let expected = checked
            .canonical_source()
            .expect("canonical source")
            .launch_contract_payload()
            .to_vec();
        let artifact = admit_checked_module(&checked).expect("admitted canonical artifact");

        let sections = wasmparser::Parser::new(0)
            .parse_all(artifact.wasm())
            .filter_map(|payload| match payload.expect("valid admitted Wasm") {
                wasmparser::Payload::CustomSection(section) if section.name() == "witchy.launch" => {
                    Some(section.data().to_vec())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(sections, vec![expected], "the admitted bytes carry the checked launch payload exactly once");

        let mut mutated = artifact.wasm().to_vec();
        *mutated.last_mut().expect("canonical launch section is nonempty") ^= 1;
        wasmparser::validate(&mutated).expect("single-byte payload mutation remains structurally valid Wasm");
        assert!(artifact.verify_exact_bytes(&mutated).is_err(), "post-admission bytes must be refused");
    }

    #[test]
    fn checked_source_witness_cannot_substitute_between_modules() {
        let first = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), b"fn main() -> Int:\n    40 + 2\n".to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("first checked source");
        let second = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), b"fn main() -> Int:\n    41 + 1\n".to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("second checked source");
        let validated = validate_checked_module(&first).expect("first module validates");
        let error = match admit_canonical(
            validated,
            &(second.canonical_source().expect("second canonical witness")).view(),
        ) {
            Ok(_) => panic!("a different source witness must not admit"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            CanonicalAdmissionError::SourceWitnessMismatch
        ));
    }

    #[test]
    fn production_entry_emits_only_admitted_canonical_bytes() {
        let source = "fn main() -> Int:\n    40 + 2\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked source");

        let artifact = crate::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("public production entry must preserve admission authority");
        let bytes = crate::codegen::compile_checked_module_binary(&checked)
            .expect_lowered("public production entry must emit Wasm");
        assert_eq!(bytes, artifact.wasm());
        wasmparser::validate(&bytes).expect("public production bytes must be valid Wasm");

        let mut config = wasmtime::Config::new();
        config.wasm_reference_types(true);
        config.wasm_function_references(true);
        config.wasm_gc(true);
        let engine = wasmtime::Engine::new(&config)
            .expect("configure public production Wasm engine");
        let module = wasmtime::Module::new(&engine, &bytes)
            .expect("compile public production Wasm");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
            .expect("instantiate public production Wasm");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f0")
            .expect("canonical production main export");
        assert_eq!(main.call(&mut store, ()).expect("run public production Wasm"), 42);
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
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
            .expect("instantiate bounds-checked List push artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f0")
            .expect("canonical main export");

        let error = main.call(&mut store, ()).expect_err("logical-length OOB access must terminate");
        assert_eq!(error.root_cause().to_string(),
            "runtime error: `main`, line 4: list index 2 out of bounds (length 2)");
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
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
            .expect("instantiate admitted bounds-checked artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f0")
            .expect("canonical main export");

        let error = main.call(&mut store, ()).expect_err("admitted OOB access must terminate");
        assert_eq!(error.root_cause().to_string(),
            "runtime error: `main`, line 2: list index 1 out of bounds (length 1)");
    }

    #[test]
    fn shared_callable_cannot_satisfy_unique_function_value_contract() {
        let source = "mode opt\n\nfn shared(var values: List(Int), value: Int) -> ():\n    values.push(value)\n\nfn apply(callback: fn(var unique List(Int), Int) -> (), var values: unique List(Int)) -> ():\n    callback(values, 3)\n\nfn main() -> Int:\n    var values: unique List(Int) = [1, 2]\n    apply(shared, values)\n    values.length()\n";
        let error = match witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ) {
            Ok(_) => panic!("an ordinary callable must not satisfy var unique callback contract"),
            Err(error) => error,
        };
        assert!(
            format!("{error:?}").contains("does not preserve Qualifier"),
            "expected the checker to reject the erased unique callback contract, got {error:?}"
        );
    }


    struct FixtureConsole;

    fn fixture_console_print(
        mut caller: wasmtime::Caller<'_, Vec<u8>>,
        token: Option<wasmtime::Rooted<wasmtime::ExternRef>>,
        pointer: i32,
        length: i32,
    ) {
        let token = token.expect("actual fixture Console token");
        assert!(token.data(&caller).expect("live token").expect("host token")
            .is::<FixtureConsole>());
        let memory = caller.get_export("memory")
            .and_then(wasmtime::Extern::into_memory).expect("canonical memory export");
        let start = usize::try_from(pointer).expect("non-negative print pointer");
        let length = usize::try_from(length).expect("non-negative print length");
        let bytes = memory.data(&caller)[start..start + length].to_vec();
        caller.data_mut().extend_from_slice(&bytes);
    }

    #[test]
    fn list_string_literal_reaches_final_safewir_validation() {
        let source = "fn main() -> Int:\n    let xs = [\"a\", \"b\"]\n    1\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked List(String) source");
        let canonical = checked.canonical_source().expect("canonical source");
        let layouts = derive_layout_bundle(canonical).expect("canonical pointer-list layouts");
        let candidate =
            lower_typed_module(canonical, layouts).expect("canonical List(String) lowering");
        let operations = candidate
            .functions()
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.operations.iter())
            .collect::<Vec<_>>();
        assert_eq!(
            operations
                .iter()
                .filter(|operation| matches!(
                    operation,
                    witchy_wir::canonical_safewir::Operation::Store {
                        transfer: witchy_wir::canonical_safewir::StoreTransfer::MoveUnique,
                        ..
                    }
                ))
                .count(),
            2
        );
        let mut prefix = 0_u64;
        for (index, operation) in operations.iter().enumerate() {
            if let witchy_wir::canonical_safewir::Operation::Store {
                place,
                transfer: witchy_wir::canonical_safewir::StoreTransfer::MoveUnique,
                ..
            } = operation {
                prefix += 1;
                let witchy_wir::canonical_safewir::Operation::ConstSize { destination, value } =
                    operations[index + 1] else { panic!("owned store must publish its prefix") };
                assert_eq!(*value, prefix);
                assert!(matches!(operations[index + 2],
                    witchy_wir::canonical_safewir::Operation::UpdateCount { object, count }
                    if *object == place.object() && count == destination),
                    "publish before evaluating another element or entering cleanup");
            }
        }
        validate_checked_module(&checked)
            .expect("List(String) reaches final SafeWIR validation");
    }

    #[test]
    fn list_string_index_reaches_final_safewir_validation() {
        let source =
            "fn main(console: Console):\n    let xs = [\"first\", \"second\"]\n    console.print(xs[1])\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked indexed List(String) source");
        let canonical = checked.canonical_source().expect("canonical source");
        let layouts = derive_layout_bundle(canonical).expect("canonical pointer-list layouts");
        let candidate =
            lower_typed_module(canonical, layouts).expect("canonical List(String) index lowering");
        assert!(candidate.functions()[0].blocks.iter()
            .flat_map(|block| &block.operations)
            .any(|operation| matches!(
                operation,
                witchy_wir::canonical_safewir::Operation::Load {
                    transfer: witchy_wir::canonical_safewir::LoadTransfer::CloneShared,
                    place,
                    ..
                } if matches!(place.projection(), witchy_wir::canonical_safewir::Projection::Element { .. })
            )));
        let initial = witchy_wir::canonical_safewir::check_initial(candidate.clone());
        assert!(initial.is_ok(), "{initial:?}\n{candidate:#?}");
        validate_checked_module(&checked)
            .expect("indexed List(String) reaches final SafeWIR validation");

        let artifact = admit_checked_module(&checked)
            .expect("admit indexed List(String) artifact");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted indexed List(String) artifact");
        let mut linker = fixture_diagnostic_linker(&engine);
        linker.func_wrap("witchy", "console_print", fixture_console_print).expect("bind authenticated Console ABI");
        let mut store = wasmtime::Store::new(&engine, Vec::<u8>::new());
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate admitted indexed List(String) artifact");
        let main = instance
            .get_typed_func::<Option<wasmtime::Rooted<wasmtime::ExternRef>>, ()>(
                &mut store,
                "f0",
            )
            .expect("canonical indexed List(String) main export");
        let console = wasmtime::ExternRef::new(&mut store, FixtureConsole).expect("fixture Console token");
        main.call(&mut store, Some(console))
            .expect("run canonical indexed List(String)");
        assert_eq!(store.data(), b"second");
    }

    #[test]
    fn list_string_push_moves_owned_elements_and_executes() {
        let source = "fn main(console: Console):\n    var xs = [\"first\"]\n    xs.push(\"second\")\n    console.print(xs[1])\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked List(String) push source");
        let canonical = checked.canonical_source().expect("canonical source");
        let layouts = derive_layout_bundle(canonical).expect("canonical pointer-list layouts");
        let candidate = lower_typed_module(canonical, layouts)
            .expect("canonical List(String) push lowering");
        let initial = witchy_wir::canonical_safewir::check_initial(candidate.clone());
        assert!(initial.is_ok(), "{initial:?}\n{candidate:#?}");
        let artifact = admit_checked_module(&checked)
            .expect("admit List(String) push artifact");

        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted List(String) push artifact");
        let mut linker = fixture_diagnostic_linker(&engine);
        linker.func_wrap("witchy", "console_print", fixture_console_print).expect("bind authenticated Console ABI");
        let mut store = wasmtime::Store::new(&engine, Vec::<u8>::new());
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate admitted List(String) push artifact");
        let main = instance
            .get_typed_func::<Option<wasmtime::Rooted<wasmtime::ExternRef>>, ()>(
                &mut store,
                "f0",
            )
            .expect("canonical List(String) push main export");
        let console = wasmtime::ExternRef::new(&mut store, FixtureConsole).expect("fixture Console token");
        main.call(&mut store, Some(console))
            .expect("run canonical List(String) push");
        assert_eq!(store.data(), b"second");
    }

}

#[cfg(test)]
#[path = "canonical_vm_service_tests.rs"]
mod vm_service_tests;
