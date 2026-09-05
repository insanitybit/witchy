//! Internal RFC-0165 stage composition.
//!
//! This is intentionally crate-private until its inputs come directly from a
//! `CheckedModule` canonical source product. It produces final SafeWIR authority, never
//! bytes, and has no raw-WIR fallback.

use std::fmt;

use witchy_types::pipeline::CheckedModule;
use witchy_wir::canonical_safewir::{
    FinalValidationError, InitialWitnessError,
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
    InitialValidation(InitialWitnessError),
    FinalValidation(FinalValidationError),
    Admission(CanonicalAdmissionError),
}

pub(crate) fn admit_checked_module(
    checked: &CheckedModule,
) -> Result<AdmittedCanonicalArtifact, CanonicalPipelineError> {
    let source = checked
        .canonical_source()
        .ok_or(CanonicalPipelineError::MissingCheckedSourceWitness)?;
    let validated = validate_checked_module(checked)?;
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
    let layouts = derive_layout_bundle(source).map_err(CanonicalPipelineError::Layout)?;
    let candidate = lower_typed_module(source, layouts)
        .map_err(CanonicalPipelineError::Lowering)?;
    let checked = check_initial_with_witness(candidate, source)
        .map_err(CanonicalPipelineError::InitialValidation)?;
    // RFC-0165 milestone one has no optional transformations. The final
    // validator consumes and independently rechecks this exact sealed graph.
    validate_final(checked, source)
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
        assert_eq!(validated.revision().get(), 0);
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
        assert_eq!(host_call.len(), 1);
        let witchy_wir::canonical_safewir::HostCallArgument::Buffer { place, extent } = &host_call[0]
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
        let mut linker = wasmtime::Linker::new(&engine);
        linker
            .func_wrap(
                "witchy",
                "print",
                |mut caller: wasmtime::Caller<'_, Vec<u8>>, pointer: i32, length: i32| {
                    let memory = caller
                        .get_export("memory")
                        .and_then(wasmtime::Extern::into_memory)
                        .expect("canonical memory export");
                    let start = usize::try_from(pointer).expect("non-negative print pointer");
                    let length = usize::try_from(length).expect("non-negative print length");
                    let bytes = memory.data(&caller)[start..start + length].to_vec();
                    caller.data_mut().extend_from_slice(&bytes);
                },
            )
            .expect("bind authenticated print ABI");
        let mut store = wasmtime::Store::new(&engine, Vec::<u8>::new());
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate admitted console.print artifact");
        let main = instance
            .get_typed_func::<Option<wasmtime::Rooted<wasmtime::ExternRef>>, ()>(&mut store, "f0")
            .expect("canonical console main export");
        main.call(&mut store, None).expect("run canonical console.print");
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
        let mut linker = wasmtime::Linker::new(&engine);
        linker
            .func_wrap(
                "witchy",
                "print",
                |mut caller: wasmtime::Caller<'_, Vec<u8>>, pointer: i32, length: i32| {
                    let memory = caller
                        .get_export("memory")
                        .and_then(wasmtime::Extern::into_memory)
                        .expect("canonical memory export");
                    let start = usize::try_from(pointer).expect("non-negative print pointer");
                    let length = usize::try_from(length).expect("non-negative print length");
                    let bytes = memory.data(&caller)[start..start + length].to_vec();
                    caller.data_mut().extend_from_slice(&bytes);
                },
            )
            .expect("bind authenticated print ABI");
        let mut store = wasmtime::Store::new(&engine, Vec::<u8>::new());
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate admitted RFC-0165 list-for artifact");
        let main = instance
            .get_typed_func::<Option<wasmtime::Rooted<wasmtime::ExternRef>>, ()>(&mut store, "f0")
            .expect("canonical RFC-0165 list-for main export");
        main.call(&mut store, None)
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
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
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
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
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
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
            .expect("instantiate public production Wasm");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f0")
            .expect("canonical production main export");
        assert_eq!(main.call(&mut store, ()).expect("run public production Wasm"), 42);
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
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
            .expect("instantiate canonical var write-back artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f1")
            .expect("canonical var write-back main export");
        assert_eq!(main.call(&mut store, ()).expect("run canonical var write-back main"), 42);
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
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
            .expect("instantiate admitted own List identity artifact");
        let main = instance
            .get_typed_func::<(), i64>(&mut store, "f1")
            .expect("canonical own List main export");
        assert_eq!(main.call(&mut store, ()).expect("run canonical own List main"), 42);
    }

    #[test]
    fn own_list_forwarded_entry_parameter_reaches_admitted_execution() {
        let source = "fn identity(own xs: List(Int)) -> List(Int):\n    xs\nfn forward(own xs: List(Int)) -> Int:\n    let ys = identity(xs)\n    ys[0]\nfn main() -> Int:\n    forward([42])\n";
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
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
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
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("instantiate indirect own List artifact");
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
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("instantiate indirect let List artifact");
        let main = instance.get_typed_func::<(), i64>(&mut store, "f2").expect("indirect let List main export");
        assert_eq!(main.call(&mut store, ()).expect("run indirect let List main"), 42);
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
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("instantiate indirect own List tail artifact");
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
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
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
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
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
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
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
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
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
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
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
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
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
        let instance = wasmtime::Instance::new(&mut store, &module, &[])
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
        let mut linker = wasmtime::Linker::new(&engine);
        linker
            .func_wrap(
                "witchy",
                "print",
                |mut caller: wasmtime::Caller<'_, Vec<u8>>, pointer: i32, length: i32| {
                    let memory = caller
                        .get_export("memory")
                        .and_then(wasmtime::Extern::into_memory)
                        .expect("canonical memory export");
                    let start = usize::try_from(pointer).expect("non-negative print pointer");
                    let length = usize::try_from(length).expect("non-negative print length");
                    let bytes = memory.data(&caller)[start..start + length].to_vec();
                    caller.data_mut().extend_from_slice(&bytes);
                },
            )
            .expect("bind authenticated print ABI");
        let mut store = wasmtime::Store::new(&engine, Vec::<u8>::new());
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate admitted direct String-result artifact");
        let main = instance
            .get_typed_func::<Option<wasmtime::Rooted<wasmtime::ExternRef>>, ()>(&mut store, "f1")
            .expect("canonical main export");
        main.call(&mut store, None)
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
        assert!(candidate.functions()[0].blocks[0]
            .operations
            .iter()
            .any(|operation| matches!(
                operation,
                witchy_wir::canonical_safewir::Operation::Load {
                    transfer: witchy_wir::canonical_safewir::LoadTransfer::CloneShared,
                    ..
                }
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
        let mut linker = wasmtime::Linker::new(&engine);
        linker
            .func_wrap(
                "witchy",
                "print",
                |mut caller: wasmtime::Caller<'_, Vec<u8>>, pointer: i32, length: i32| {
                    let memory = caller
                        .get_export("memory")
                        .and_then(wasmtime::Extern::into_memory)
                        .expect("canonical memory export");
                    let start = usize::try_from(pointer).expect("non-negative print pointer");
                    let length = usize::try_from(length).expect("non-negative print length");
                    let bytes = memory.data(&caller)[start..start + length].to_vec();
                    caller.data_mut().extend_from_slice(&bytes);
                },
            )
            .expect("bind authenticated print ABI");
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
        main.call(&mut store, None)
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
        let mut linker = wasmtime::Linker::new(&engine);
        linker
            .func_wrap(
                "witchy",
                "print",
                |mut caller: wasmtime::Caller<'_, Vec<u8>>, pointer: i32, length: i32| {
                    let memory = caller
                        .get_export("memory")
                        .and_then(wasmtime::Extern::into_memory)
                        .expect("canonical memory export");
                    let start = usize::try_from(pointer).expect("non-negative print pointer");
                    let length = usize::try_from(length).expect("non-negative print length");
                    let bytes = memory.data(&caller)[start..start + length].to_vec();
                    caller.data_mut().extend_from_slice(&bytes);
                },
            )
            .expect("bind authenticated print ABI");
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
        main.call(&mut store, None)
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
        let mut linker = wasmtime::Linker::new(&engine);
        linker
            .func_wrap(
                "witchy",
                "print",
                |mut caller: wasmtime::Caller<'_, Vec<u8>>, pointer: i32, length: i32| {
                    let memory = caller
                        .get_export("memory")
                        .and_then(wasmtime::Extern::into_memory)
                        .expect("canonical memory export");
                    let start = usize::try_from(pointer).expect("non-negative print pointer");
                    let length = usize::try_from(length).expect("non-negative print length");
                    let bytes = memory.data(&caller)[start..start + length].to_vec();
                    caller.data_mut().extend_from_slice(&bytes);
                },
            )
            .expect("bind authenticated print ABI");
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
        main.call(&mut store, None)
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
        let mut linker = wasmtime::Linker::new(&engine);
        linker
            .func_wrap(
                "witchy",
                "print",
                |mut caller: wasmtime::Caller<'_, Vec<u8>>, pointer: i32, length: i32| {
                    let memory = caller
                        .get_export("memory")
                        .and_then(wasmtime::Extern::into_memory)
                        .expect("canonical memory export");
                    let start = usize::try_from(pointer).expect("non-negative print pointer");
                    let length = usize::try_from(length).expect("non-negative print length");
                    let bytes = memory.data(&caller)[start..start + length].to_vec();
                    caller.data_mut().extend_from_slice(&bytes);
                },
            )
            .expect("bind authenticated print ABI");
        let mut store = wasmtime::Store::new(&engine, Vec::<u8>::new());
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate admitted UTF-8 concat artifact");
        let main = instance
            .get_typed_func::<Option<wasmtime::Rooted<wasmtime::ExternRef>>, ()>(&mut store, "f0")
            .expect("canonical concat main export");
        main.call(&mut store, None).expect("run canonical UTF-8 concat");
        assert_eq!(store.data(), b"hi!");
    }
}
