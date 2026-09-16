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
    let validated = validate_checked_source(source)?;
    witchy_wir::admit_canonical_with_observer(validated, source, observe)
        .map_err(CanonicalPipelineError::Admission)
}

pub(crate) fn admit_checked_source(
    source: &CanonicalCheckedSource,
) -> Result<AdmittedCanonicalArtifact, CanonicalPipelineError> {
    let validated = validate_checked_source(source)?;
    admit_canonical(validated, source).map_err(CanonicalPipelineError::Admission)
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
    let layouts = derive_layout_bundle(source).map_err(CanonicalPipelineError::Layout)?;
    let candidate = lower_typed_module(source, layouts)
        .map_err(CanonicalPipelineError::Lowering)?
        .with_launch_contract_payload(source.launch_contract_payload().to_vec());
    let checked = check_initial_with_witness(candidate, source)
        .map_err(CanonicalPipelineError::InitialValidation)?;
    if source
        .typed_module()
        .module()
        .modes
        .iter()
        .any(|mode| mode == "opt")
    {
        run_checked_pass(checked, PassId::SameBlockBoundsGuards).validate_final(source)
            .map_err(CanonicalPipelineError::FinalValidation)
    } else {
        // Ordinary mode remains the zero-pass baseline. The final validator
        // consumes and independently rechecks this exact sealed graph.
        validate_final(checked, source).map_err(CanonicalPipelineError::FinalValidation)
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
        assert_eq!(validated.revision().get(), 0);
    }

    #[test]
    fn opt_mode_runs_the_assured_bounds_pass_before_admission() {
        let source = "mode opt\n\nfn main() -> Int:\n    let xs = [41]\n    xs[0]\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked opt source");
        let validated = validate_checked_module(&checked)
            .expect("assured bounds optimization reaches final validation");
        assert_eq!(
            validated.revision().get(),
            1,
            "the source-generated redundant bounds guard must be removed"
        );

        let artifact = admit_checked_module(&checked).expect("optimized opt module admits");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm()).expect("optimized Wasm");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = fixture_diagnostic_linker(&engine)
            .instantiate(&mut store, &module)
            .expect("instantiate optimized Wasm");
        let entry = artifact.entry().expect("checked main entry").export();
        let main = instance
            .get_typed_func::<(), i64>(&mut store, entry)
            .expect("typed optimized entry");
        assert_eq!(main.call(&mut store, ()).expect("run optimized entry"), 41);
    }

    #[test]
    fn normal_caller_prepares_owned_value_for_opt_export_without_changing_value_semantics() {
        let api = "mode opt\n\npub fn append(own values: List(Int), value: Int) -> List(Int):\n    values.push(value)\n    values\n";
        let main = "import api\n\nfn main() -> Int:\n    let values = [1, 2]\n    let extended = api.append(values, 3)\n    extended.length() * 10 + values[0]\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![
                ("api".to_string(), api.as_bytes().to_vec()),
                ("main".to_string(), main.as_bytes().to_vec()),
            ],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("normal source may call a conventional owned opt export");

        let artifact = admit_checked_module(&checked)
            .expect("normal-to-opt ownership preparation reaches canonical admission");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("normal-to-opt artifact is valid Wasm");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = fixture_diagnostic_linker(&engine)
            .instantiate(&mut store, &module)
            .expect("instantiate normal-to-opt artifact");
        let entry = artifact.entry().expect("checked main entry").export();
        let main = instance
            .get_typed_func::<(), i64>(&mut store, entry)
            .expect("typed normal-to-opt entry");

        assert_eq!(
            main.call(&mut store, ()).expect("run normal-to-opt entry"),
            31,
            "the opt callee receives an owned value while the normal caller retains its value"
        );
    }

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
    fn shared_list_string_identity_reaches_final_safewir_authority() {
        let source = "fn identity(xs: List(String)) -> List(String):\n    xs\n\nfn main() -> Int:\n    identity([\"x\"]).length()\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked source");

        validate_checked_module(&checked)
            .expect("shared List(String) identity return must be admitted");
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
            second.canonical_source().expect("second canonical witness"),
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
    fn console_print_reaches_admitted_execution() {
        let source = "fn main(console: Console):\n    console.print(\"ok\")\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked source");

        let canonical = checked.canonical_source().expect("canonical source");
        let layouts = derive_layout_bundle(canonical).expect("canonical layouts");
        let candidate = lower_typed_module(canonical, layouts).expect("canonical lowering");
        let host_call = candidate.functions()[0].blocks[0]
            .operations
            .iter()
            .find_map(|operation| match operation {
                witchy_wir::canonical_safewir::Operation::HostCall { arguments, .. } => {
                    Some(arguments)
                }
                _ => None,
            })
            .expect("console.print HostCall");
        assert_eq!(host_call.len(), 2);
        let witchy_wir::canonical_safewir::HostCallArgument::Capability(receiver) = &host_call[0]
        else { panic!("console.print must pass its actual receiver"); };
        assert_eq!(*receiver, candidate.functions()[0].blocks[0].parameters[0].value);
        let witchy_wir::canonical_safewir::HostCallArgument::Buffer { place, extent } = &host_call[1]
        else {
            panic!("console.print must pass a checked buffer");
        };
        assert_eq!(*extent, witchy_wir::canonical_safewir::HostBufferExtent::Slice);
        let witchy_wir::canonical_safewir::Projection::Slice { start, length } = place.projection()
        else {
            panic!("console.print must pass a UTF-8 body slice");
        };
        assert_ne!(start, length);
        assert!(candidate.functions()[0].blocks[0].operations.iter().any(|operation| {
            matches!(operation, witchy_wir::canonical_safewir::Operation::ConstSize {
                destination, value: 0
            } if destination == start)
        }));

        validate_checked_module(&checked).expect("console.print reaches final SafeWIR validation");
        let artifact = admit_checked_module(&checked).expect("admit canonical console.print artifact");
        wasmparser::validate(artifact.wasm()).expect("admitted console.print bytes remain valid Wasm");

        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted console.print artifact");
        let mut linker = fixture_diagnostic_linker(&engine);
        linker.func_wrap("witchy", "console_print", fixture_console_print).expect("bind authenticated Console ABI");
        let mut store = wasmtime::Store::new(&engine, Vec::<u8>::new());
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate admitted console.print artifact");
        let main = instance
            .get_typed_func::<Option<wasmtime::Rooted<wasmtime::ExternRef>>, ()>(&mut store, "f0")
            .expect("canonical console main export");
        let console = wasmtime::ExternRef::new(&mut store, FixtureConsole).expect("fixture Console token");
        main.call(&mut store, Some(console)).expect("run canonical console.print");
        assert_eq!(store.data(), b"ok");
    }

    #[test]
    fn rfc0165_list_for_push_reaches_admitted_execution() {
        let source = "fn main(console: Console):\n    var out = []\n    for n in [1, 2]:\n        out.push(\"x\")\n    for value in out:\n        console.print(value)\n    console.print(out[1])\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked RFC-0165 list-for source");
        let artifact = admit_checked_module(&checked)
            .expect("admit canonical RFC-0165 list-for artifact");

        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted RFC-0165 list-for artifact");
        let mut linker = fixture_diagnostic_linker(&engine);
        linker.func_wrap("witchy", "console_print", fixture_console_print).expect("bind authenticated Console ABI");
        let mut store = wasmtime::Store::new(&engine, Vec::<u8>::new());
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate admitted RFC-0165 list-for artifact");
        let main = instance
            .get_typed_func::<Option<wasmtime::Rooted<wasmtime::ExternRef>>, ()>(&mut store, "f0")
            .expect("canonical RFC-0165 list-for main export");
        let console = wasmtime::ExternRef::new(&mut store, FixtureConsole).expect("fixture Console token");
        main.call(&mut store, Some(console))
            .expect("run canonical RFC-0165 list-for artifact");
        assert_eq!(store.data(), b"xxx");
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
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
            .expect("instantiate admitted canonical artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f0")
            .expect("canonical main export");
        assert_eq!(main.call(&mut store, ()).expect("run canonical main"), 42);
    }

    #[test]
    fn scalar_unary_source_reaches_admitted_execution() {
        let source = "fn main() -> Int:\n    let x = -40\n    if !false:\n        -x + 2\n    else:\n        ~0\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked unary source");
        let artifact = admit_checked_module(&checked).expect("admit canonical unary artifact");

        wasmparser::validate(artifact.wasm()).expect("unary artifact is valid Wasm");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile canonical unary artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
            .expect("instantiate canonical unary artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f0")
            .expect("canonical unary main export");
        assert_eq!(main.call(&mut store, ()).expect("run unary main"), 42);
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

    /// A library generic instantiated at a program type checks as source but
    /// is outside canonical v1: checked-source construction succeeds and the
    /// canonical route refuses at lowering, naming the specialization.
    #[test]
    fn library_specialization_reachable_from_main_admits() {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![
                (
                    "main".to_string(),
                    b"import util\n\nfn main() -> Int:\n    util.identity(1)\n".to_vec(),
                ),
                ("util".to_string(), b"pub fn identity(value: a) -> a:\n    value\n".to_vec()),
            ],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked source construction must not refuse a valid program");

        // The library instance is reachable from `main`, so it is part of the
        // canonical scope and the program admits; an unreachable instance
        // would still be a digest-bound obligation refused at lowering.
        admit_checked_module(&checked).expect("a reachable library specialization lowers and admits");
        assert!(matches!(
            crate::codegen::compile_checked_module_artifact(&checked),
            crate::codegen::LoweringOutcome::Lowered(_)
        ));
    }

    /// The linker leaves `main` unqualified and qualifies the entry module's
    /// other functions (`app.helper`). A program whose file is not named
    /// `main` must still admit both, and the artifact must record a launch
    /// entry, or the CLI cannot run any canonical program.
    #[test]
    fn non_main_module_keeps_bare_main_and_records_a_launch_entry() {
        let source = "fn helper() -> Int:\n    41\n\nfn main() -> Int:\n    helper() + 1\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("app".to_string(), source.as_bytes().to_vec())],
            "app",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked source");
        let canonical = checked.canonical_source().expect("canonical source");
        assert!(canonical.callable_signature("main").is_some(), "bare `main` is in the entry scope");
        assert!(canonical.callable_signature("app.helper").is_some(), "qualified helpers are in the entry scope");

        let artifact = admit_checked_module(&checked).expect("admitted canonical artifact");
        let entry = artifact.entry().expect("a program with `main` records its launch entry");
        assert!(entry.capabilities().is_empty());
        assert_eq!(entry.result(), witchy_wir::CanonicalEntryResult::Int);
        assert!(entry.export().starts_with('f'));
    }

    /// A scalar `match` with a wildcard arm is inside the foundation subset.
    /// The lowering once allocated a fallthrough block for the wildcard that
    /// nothing branched to, and the structured backend refused the dead block
    /// at admission (`hello`, `compute`, `signs`).
    #[test]
    fn scalar_match_with_wildcard_reaches_admission() {
        let source = "fn step(direction: Int) -> Int:\n    match direction:\n        -1 -> 10\n        1 -> 20\n        _ -> 0\n\nfn main() -> Int:\n    step(-1) + step(1) + step(7)\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("signs".to_string(), source.as_bytes().to_vec())],
            "signs",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked source");
        let artifact = admit_checked_module(&checked).expect("a wildcard match admits");
        wasmparser::validate(artifact.wasm()).expect("admitted bytes are valid Wasm");
        assert!(artifact.entry().is_some());
    }

    /// An explicit `let` parameter is the default convention spelled out; the
    /// lowering once hit `unreachable!()` on it (`examples/regions`).
    #[test]
    fn explicit_let_parameter_lowers_like_the_default_convention() {
        let source = "fn twice(let n: Int) -> Int:\n    n * 2\n\nfn first(let xs: List(Int)) -> Int:\n    xs.length()\n\nfn main() -> Int:\n    twice(20) + first([1, 2])\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("regions".to_string(), source.as_bytes().to_vec())],
            "regions",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked source");
        let artifact = admit_checked_module(&checked).expect("explicit `let` parameters admit");
        wasmparser::validate(artifact.wasm()).expect("admitted bytes are valid Wasm");
    }

    /// An exhaustive `match` without a wildcard leaves a trapping fallthrough
    /// as one side of the last `BranchIf`; the structured backend must treat
    /// that terminal side as bypassing the join rather than vetoing every
    /// candidate join.
    #[test]
    fn bool_match_without_wildcard_reaches_admission() {
        let source = "fn choose(flag: Bool) -> Int:\n    match flag:\n        true -> 1\n        false -> 2\n\nfn main() -> Int:\n    choose(true) + choose(false)\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("choose".to_string(), source.as_bytes().to_vec())],
            "choose",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked source");
        let artifact = admit_checked_module(&checked).expect("an exhaustive match admits");
        wasmparser::validate(artifact.wasm()).expect("admitted bytes are valid Wasm");
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
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
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
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
            .expect("instantiate admitted parameterized-call artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f1")
            .expect("canonical main export");

        assert_eq!(main.call(&mut store, ()).expect("run canonical main"), 42);
    }

    #[test]
    fn var_list_write_back_replacement_reaches_admitted_execution() {
        let source = "fn replace(var xs: List(Int)):\n    xs = [42]\n\nfn main() -> Int:\n    var xs = [1]\n    replace(xs)\n    xs[0]\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked var write-back source");
        let artifact = admit_checked_module(&checked)
            .expect("admit canonical var write-back artifact");

        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile canonical var write-back artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
            .expect("instantiate canonical var write-back artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f1")
            .expect("canonical var write-back main export");
        assert_eq!(main.call(&mut store, ()).expect("run canonical var write-back main"), 42);
    }

    #[test]
    fn branch_local_list_headers_do_not_escape_to_other_writeback_edges() {
        for body in [
            "    match flag:\n        true ->\n            let first = xs[0]\n            xs = [first + 41]\n        false -> Nil\n",
            "    if flag:\n        let first = xs[0]\n        xs = [first + 41]\n    else:\n        Nil\n",
        ] {
            let source = format!("fn update(flag: Bool, var xs: List(Int)):\n{body}\nfn main() -> Int:\n    var a = [1]\n    var b = [2]\n    update(true, a)\n    update(false, b)\n    a[0] + b[0]\n");
            let checked = witchy_types::pipeline::link_checked_from_source_bundle(
                vec![("main".to_string(), source.into_bytes())],
                "main", |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
            ).expect("checked branch-local headers");
            let artifact = admit_checked_module(&checked).expect("admit branch-local headers");
            let engine = wasmtime::Engine::default();
            let module = wasmtime::Module::new(&engine, artifact.wasm()).expect("compile branch-local headers");
            let mut store = wasmtime::Store::new(&engine, ());
            let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module).expect("instantiate branch-local headers");
            let main = instance.get_typed_func::<(), i64>(&mut store, "f1").expect("branch-local headers main");
            assert_eq!(main.call(&mut store, ()).expect("run branch-local headers"), 44);
        }
    }

    #[test]
    fn own_list_identity_reaches_admitted_execution() {
        let source = "fn identity(own xs: List(Int)) -> List(Int):\n    xs\nfn main() -> Int:\n    let xs = identity([42])\n    xs[0]\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked own List identity source");
        let artifact = admit_checked_module(&checked)
            .expect("admit canonical own List identity artifact");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted own List identity artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
            .expect("instantiate admitted own List identity artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f1")
            .expect("canonical own List main export");
        assert_eq!(main.call(&mut store, ()).expect("run canonical own List main"), 42);
    }

    #[test]
    fn own_list_forwarded_entry_parameter_reaches_admitted_execution() {
        let source = "mode opt\n\nfn identity(own xs: List(Int)) -> List(Int):\n    xs\nfn forward(own xs: List(Int)) -> Int:\n    let ys = identity(xs)\n    ys[0]\nfn main() -> Int:\n    forward([42])\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked forwarded own List source");
        let canonical = checked.canonical_source().expect("canonical source");
        let candidate = lower_typed_module(
            canonical,
            derive_layout_bundle(canonical).expect("canonical layouts"),
        )
        .expect("lower forwarded own List source");
        let forward = candidate.functions().iter().find(|function| function.id.get() == 1)
            .expect("forward function");
        let entry_token = forward.blocks.iter().find(|block| block.id == forward.entry)
            .and_then(|block| block.parameters.first())
            .expect("forward own entry parameter")
            .value;
        assert!(!forward.blocks.iter().flat_map(|block| &block.operations).any(|operation| {
            matches!(operation, witchy_wir::canonical_safewir::Operation::DropRef { object } if *object == entry_token)
        }), "a moved own entry token must not receive local cleanup");

        let artifact = admit_checked_module(&checked)
            .expect("admit canonical forwarded own List artifact");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted forwarded own List artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
            .expect("instantiate admitted forwarded own List artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f2")
            .expect("canonical forwarded own List main export");
        assert_eq!(main.call(&mut store, ()).expect("run canonical forwarded own List main"), 42);
    }

    #[test]
    fn indirect_own_list_call_reaches_admitted_execution() {
        let source = "fn identity(own xs: List(Int)) -> List(Int):\n    xs\nfn apply(f: fn(own List(Int)) -> List(Int), own xs: List(Int)) -> Int:\n    let ys = f(xs)\n    ys[0]\nfn main() -> Int:\n    apply(identity, [42])\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main", |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked indirect own List source");
        let artifact = admit_checked_module(&checked).expect("admit indirect own List artifact");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm()).expect("compile indirect own List artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module).expect("instantiate indirect own List artifact");
        let main = instance.get_typed_func::<(), i64>(&mut store, "f2").expect("indirect own List main export");
        assert_eq!(main.call(&mut store, ()).expect("run indirect own List main"), 42);
    }

    #[test]
    fn indirect_let_list_call_reaches_admitted_execution() {
        let source = "fn identity(xs: List(Int)) -> List(Int):\n    xs\nfn apply(f: fn(List(Int)) -> List(Int), xs: List(Int)) -> Int:\n    let ys = f(xs)\n    ys[0]\nfn main() -> Int:\n    apply(identity, [42])\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main", |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked indirect let List source");
        let artifact = admit_checked_module(&checked).expect("admit indirect let List artifact");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm()).expect("compile indirect let List artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module).expect("instantiate indirect let List artifact");
        let main = instance.get_typed_func::<(), i64>(&mut store, "f2").expect("indirect let List main export");
        assert_eq!(main.call(&mut store, ()).expect("run indirect let List main"), 42);
    }

    #[test]
    fn indirect_var_list_replacements_preserve_caller_aliases() {
        let source = "fn replace(var xs: List(Int), var ys: List(Int)) -> Bool:\n    xs = [42]\n    ys = [7]\n    true\nfn apply(f: fn(var List(Int), var List(Int)) -> Bool, var xs: List(Int), var ys: List(Int)) -> Bool:\n    let changed = f(xs, ys)\n    changed\nfn main() -> Int:\n    var xs = [1]\n    var ys = [2]\n    let old = xs\n    let changed = apply(replace, xs, ys)\n    if changed: old[0] * 100 + xs[0] + ys[0] else: 0\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main", |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked indirect var replacements");
        let artifact = admit_checked_module(&checked).expect("admit indirect var replacements");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm()).expect("compile indirect var replacements");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module).expect("instantiate indirect var replacements");
        let main = instance.get_typed_func::<(), i64>(&mut store, "f2").expect("var replacements main");
        assert_eq!(main.call(&mut store, ()).expect("run var replacements"), 149);
    }

    #[test]
    fn indirect_own_list_tail_call_reaches_admitted_execution() {
        let source = "fn identity(own xs: List(Int)) -> List(Int):\n    xs\nfn apply(f: fn(own List(Int)) -> List(Int), own xs: List(Int)) -> List(Int):\n    f(xs)\nfn main() -> Int:\n    apply(identity, [42])[0]\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main", |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).expect("checked indirect own List tail source");
        let canonical = checked.canonical_source().expect("canonical source");
        let candidate = lower_typed_module(canonical, derive_layout_bundle(canonical).expect("canonical layouts"))
            .expect("lower indirect own List tail source");
        assert!(matches!(candidate.functions()[1].blocks[0].terminator,
            witchy_wir::canonical_safewir::Terminator::VarTailCall { .. }));
        let artifact = admit_checked_module(&checked).expect("admit indirect own List tail artifact");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm()).expect("compile indirect own List tail artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module).expect("instantiate indirect own List tail artifact");
        let main = instance.get_typed_func::<(), i64>(&mut store, "f2").expect("indirect own List tail main export");
        assert_eq!(main.call(&mut store, ()).expect("run indirect own List tail main"), 42);
    }

    #[test]
    fn checked_function_value_variable_call_reaches_admitted_execution() {
        let source = "fn answer() -> Int:\n    42\nfn main() -> Int:\n    let f = answer\n    f() + 1\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked function-value source");

        let canonical = checked.canonical_source().expect("canonical source");
        let layouts = derive_layout_bundle(canonical).expect("canonical layouts");
        let candidate = lower_typed_module(canonical, layouts).expect("canonical lowering");
        let operations = candidate.functions()[1].blocks[0].operations.as_slice();
        assert!(operations.iter().any(|operation| {
            matches!(operation, witchy_wir::canonical_safewir::Operation::FunctionValue { .. })
        }));
        assert!(operations.iter().any(|operation| {
            matches!(operation, witchy_wir::canonical_safewir::Operation::VarCall { .. })
        }));

        let artifact = admit_checked_module(&checked)
            .expect("admit canonical function-value artifact");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted function-value artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
            .expect("instantiate admitted function-value artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f1")
            .expect("canonical main export");

        assert_eq!(main.call(&mut store, ()).expect("run canonical function value"), 43);
    }

    #[test]
    fn checked_function_value_tail_call_reaches_admitted_execution() {
        let source = "fn answer() -> Int:\n    42\nfn main() -> Int:\n    let f = answer\n    f()\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked function-value tail-call source");

        let canonical = checked.canonical_source().expect("canonical source");
        let layouts = derive_layout_bundle(canonical).expect("canonical layouts");
        let candidate = lower_typed_module(canonical, layouts).expect("canonical lowering");
        assert!(matches!(
            candidate.functions()[1].blocks[0].terminator,
            witchy_wir::canonical_safewir::Terminator::VarTailCall { .. }
        ));

        let artifact = admit_checked_module(&checked)
            .expect("admit canonical function-value tail-call artifact");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted function-value tail-call artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
            .expect("instantiate admitted function-value tail-call artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f1")
            .expect("canonical main export");

        assert_eq!(main.call(&mut store, ()).expect("run canonical function value tail call"), 42);
    }

    #[test]
    fn checked_variable_call_admits_memory_bearing_result() {
        let source = "fn answer() -> String:\n    \"ok\"\nfn main() -> Int:\n    let f = answer\n    let result = f()\n    1\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked memory-bearing function-value source");

        let artifact = admit_checked_module(&checked)
            .expect("admit authenticated memory-bearing function-value result");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile memory-bearing function-value artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
            .expect("instantiate memory-bearing function-value artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f1")
            .expect("canonical memory-bearing function-value main export");
        assert_eq!(main.call(&mut store, ()).expect("run function-value result"), 1);
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
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
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

        assert_eq!(bundle.roots().len(), 2, "List(Int) and the diagnostic source-label String");
        let (_, layouts) = witchy_wir::layout::LayoutBundle::decode_canonical(&bundle.canonical_bytes())
            .expect("decode projected layout bundle");
        let mut lists = 0;
        let mut labels = 0;
        for root in bundle.roots() {
            let descriptor = layouts.get(*root).expect("projected root descriptor");
            match descriptor.kind() {
                witchy_wir::layout::LayoutKind::PackedList { element, rc } => {
                    lists += 1;
                    assert_eq!(*rc, witchy_wir::layout::RcHeader::Required);
                    assert_eq!(layouts.get(*element).unwrap().kind(),
                        &witchy_wir::layout::LayoutKind::Scalar(witchy_wir::layout::ScalarKind::Int));
                }
                witchy_wir::layout::LayoutKind::RuntimeBuffer { element } => {
                    labels += 1;
                    assert_eq!(descriptor.size(), witchy_wir::layout::LayoutSize::Dynamic { base: 4, stride: 1 });
                    assert_eq!(layouts.get(*element).unwrap().kind(),
                        &witchy_wir::layout::LayoutKind::Scalar(witchy_wir::layout::ScalarKind::RawByte));
                }
                other => panic!("unexpected source layout {other:?}"),
            }
        }
        assert_eq!((lists, labels), (1, 1));
        let candidate = lower_typed_module(canonical, bundle).expect("lower projected list and label");
        assert_eq!(candidate.static_data_segments().len(), 1);
        assert_eq!(candidate.static_data_segments()[0].bytes(), b"\x09\0\0\0main.read");
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
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
            .expect("instantiate admitted List(Int) artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f0")
            .expect("canonical main export");

        assert_eq!(main.call(&mut store, ()).expect("run canonical main"), 20);
    }

    #[test]
    fn packed_scalar_record_field_reaches_admitted_executable_artifact() {
        let source = "type Point packed:\n    x: Int\n    y: Int\n\nfn main() -> Int:\n    let point = Point(40, 2)\n    point.y\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked packed Point source");
        let validated = validate_checked_module(&checked)
            .expect("packed Point reaches final SafeWIR authority");
        let artifact = admit_checked_module(&checked)
            .expect("admit packed Point artifact");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted packed Point artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
            .expect("instantiate admitted packed Point artifact");
        let main = instance.get_typed_func::<(), i64>(&mut store, "f0")
            .expect("canonical Point main export");
        assert_eq!(main.call(&mut store, ()).expect("run canonical Point main"), 2);
        assert_eq!(validated.checked_source().digest(), checked.canonical_source_witness().unwrap().source_bundle_digest());
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
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
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
    fn shared_list_parameter_index_reaches_admitted_execution() {
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
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
            .expect("instantiate admitted borrowed List(Int) artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f1")
            .expect("canonical main export");

        assert_eq!(main.call(&mut store, ()).expect("run canonical main"), 42);
    }

    #[test]
    fn non_tail_let_list_call_retains_the_caller_token() {
        let source = "fn first(xs: List(Int)) -> Int:\n    xs[0]\nfn main() -> Int:\n    let xs = [41]\n    let observed = first(xs)\n    observed + xs[0]\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked non-tail let List(Int) source");
        let artifact = admit_checked_module(&checked)
            .expect("admit non-tail let List(Int) artifact");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile non-tail let List(Int) artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
            .expect("instantiate non-tail let List(Int) artifact");
        let main = instance.get_typed_func::<(), i64>(&mut store, "f1")
            .expect("canonical main export");
        assert_eq!(main.call(&mut store, ()).expect("run non-tail let List(Int)"), 82);
    }

    #[test]
    fn proper_tail_let_list_call_moves_the_entry_token() {
        let source = "fn identity(xs: List(Int)) -> List(Int):\n    xs\nfn forward(xs: List(Int)) -> List(Int):\n    identity(xs)\nfn main() -> Int:\n    forward([42])[0]\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked proper-tail let List(Int) source");
        let canonical = checked.canonical_source().expect("canonical source");
        let candidate = lower_typed_module(
            canonical,
            derive_layout_bundle(canonical).expect("canonical layouts"),
        )
        .expect("lower proper-tail let List(Int) source");
        let forward = candidate.functions().iter().find(|function| function.id.get() == 1)
            .expect("forward function");
        let entry_token = forward.blocks.iter().find(|block| block.id == forward.entry)
            .and_then(|block| block.parameters.first())
            .expect("forward entry parameter")
            .value;
        assert!(matches!(forward.blocks[0].terminator,
            witchy_wir::canonical_safewir::Terminator::TailCall { ref arguments, .. }
            if matches!(arguments.as_slice(), [witchy_wir::canonical_safewir::CallArgument::Move(value)] if *value == entry_token)));
        assert!(!forward.blocks.iter().flat_map(|block| &block.operations).any(|operation| {
            matches!(operation, witchy_wir::canonical_safewir::Operation::DropRef { object } if *object == entry_token)
        }), "a tail-moved entry token cannot have residual caller cleanup");

        let artifact = admit_checked_module(&checked)
            .expect("admit proper-tail let List(Int) artifact");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile proper-tail let List(Int) artifact");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
            .expect("instantiate proper-tail let List(Int) artifact");
        let main = instance.get_typed_func::<(), i64>(&mut store, "f2")
            .expect("canonical main export");
        assert_eq!(main.call(&mut store, ()).expect("run proper-tail let List(Int)"), 42);
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
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
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
        let instance = fixture_diagnostic_linker(&engine).instantiate(&mut store, &module)
            .expect("instantiate admitted UTF-8 String literal");
        let main = instance
            .get_typed_func::<(), i32>(&mut store, "f0")
            .expect("String result export");
        assert_eq!(main.call(&mut store, ()).expect("run String literal"), 16);
    }

    #[test]
    fn body_derived_static_result_flows_through_a_direct_call() {
        let source = "fn greeting() -> String:\n    \"hello\"\nfn main(console: Console):\n    console.print(greeting())\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked direct String-result source");
        let artifact = admit_checked_module(&checked)
            .expect("body-derived static result reaches admission");

        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted direct String-result artifact");
        let mut linker = fixture_diagnostic_linker(&engine);
        linker.func_wrap("witchy", "console_print", fixture_console_print).expect("bind authenticated Console ABI");
        let mut store = wasmtime::Store::new(&engine, Vec::<u8>::new());
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate admitted direct String-result artifact");
        let main = instance
            .get_typed_func::<Option<wasmtime::Rooted<wasmtime::ExternRef>>, ()>(&mut store, "f1")
            .expect("canonical main export");
        let console = wasmtime::ExternRef::new(&mut store, FixtureConsole).expect("fixture Console token");
        main.call(&mut store, Some(console))
            .expect("run direct String-result artifact");
        assert_eq!(store.data(), b"hello");
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

    #[test]
    fn utf8_string_concat_reaches_final_safewir_validation() {
        let source = "fn worker(console: Console, suffix: String):\n    console.print(\"hi\" + suffix)\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked UTF-8 concat source");
        let canonical = checked.canonical_source().expect("canonical source");
        let layouts = derive_layout_bundle(canonical).expect("canonical layouts");
        let candidate = lower_typed_module(canonical, layouts).expect("canonical concat lowering");
        let operations = candidate
            .functions()
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.operations.iter())
            .collect::<Vec<_>>();
        assert!(operations.iter().any(|operation| matches!(
            operation,
            witchy_wir::canonical_safewir::Operation::Alloc { .. }
        )));
        assert!(operations.iter().any(|operation| matches!(
            operation,
            witchy_wir::canonical_safewir::Operation::CopyBytes { .. }
        )));
        assert!(operations.iter().any(|operation| matches!(
            operation,
            witchy_wir::canonical_safewir::Operation::UpdateCount { .. }
        )));
        validate_checked_module(&checked).expect("UTF-8 concat reaches final SafeWIR validation");
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

    #[test]
    fn pointer_list_index_result_contract_is_body_derived_and_executes() {
        let source = "fn pick() -> String:\n    let xs = [\"first\", \"second\"]\n    xs[1]\nfn main(console: Console):\n    console.print(pick())\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked pointer-list result source");
        let artifact = admit_checked_module(&checked)
            .expect("admit body-derived pointer-list result artifact");

        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted pointer-list result artifact");
        let mut linker = fixture_diagnostic_linker(&engine);
        linker.func_wrap("witchy", "console_print", fixture_console_print).expect("bind authenticated Console ABI");
        let mut store = wasmtime::Store::new(&engine, Vec::<u8>::new());
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate admitted pointer-list result artifact");
        let main = instance
            .get_typed_func::<Option<wasmtime::Rooted<wasmtime::ExternRef>>, ()>(
                &mut store,
                "f1",
            )
            .expect("canonical pointer-list result main export");
        let console = wasmtime::ExternRef::new(&mut store, FixtureConsole).expect("fixture Console token");
        main.call(&mut store, Some(console))
            .expect("run canonical pointer-list result program");
        assert_eq!(store.data(), b"second");
    }

    #[test]
    fn utf8_string_concat_reaches_admitted_execution() {
        let source = "fn main(console: Console):\n    console.print(\"hi\" + \"!\")\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked UTF-8 concat source");
        let artifact = admit_checked_module(&checked).expect("admit UTF-8 concat artifact");
        wasmparser::validate(artifact.wasm()).expect("UTF-8 concat bytes remain valid Wasm");

        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, artifact.wasm())
            .expect("compile admitted UTF-8 concat artifact");
        let mut linker = fixture_diagnostic_linker(&engine);
        linker.func_wrap("witchy", "console_print", fixture_console_print).expect("bind authenticated Console ABI");
        let mut store = wasmtime::Store::new(&engine, Vec::<u8>::new());
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate admitted UTF-8 concat artifact");
        let main = instance
            .get_typed_func::<Option<wasmtime::Rooted<wasmtime::ExternRef>>, ()>(&mut store, "f0")
            .expect("canonical concat main export");
        let console = wasmtime::ExternRef::new(&mut store, FixtureConsole).expect("fixture Console token");
        main.call(&mut store, Some(console)).expect("run canonical UTF-8 concat");
        assert_eq!(store.data(), b"hi!");
    }
}

#[cfg(test)]
#[path = "canonical_vm_service_tests.rs"]
mod vm_service_tests;
