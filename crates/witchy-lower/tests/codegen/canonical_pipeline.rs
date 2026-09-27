//! Canonical production-entry security properties, driven only through the
//! crate's public API (`compile_checked_module_artifact` /
//! `compile_checked_module_binary`) and witchy-types' public link entries.
//! Moved out of the library's `cfg(test)` modules so witchy-lower no longer
//! compiles a second test copy of itself.

use witchy_lower::codegen::{compile_checked_module_artifact, compile_checked_module_binary};
use witchy_types::pipeline::CheckedModule;

#[test]
fn task_producer_substitution_requires_actual_execution_provenance() {
    witchy_lower::canonical_pipeline_checks::task_producer_substitution_requires_actual_execution_provenance();
}

fn no_expand(
    _name: &str,
    _module: &mut witchy_syntax::ast::Module,
    _siblings: &[(String, witchy_syntax::ast::Module)],
) -> Result<witchy_syntax::origin::OriginTable, String> {
    Ok(witchy_syntax::origin::OriginTable::default())
}

fn checked(source: &str) -> CheckedModule {
    witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("main".to_string(), source.as_bytes().to_vec())],
        "main",
        witchy_syntax::linker::ComptimeExpander::new(no_expand),
    )
    .expect("checked source")
}

/// Deliberately link only the exact canonical diagnostic ABIs. Unknown imports
/// or mismatched signatures must still fail instantiation. These imports
/// always terminate; a no-op host would conceal failed guards.
#[cfg(feature = "native-test-runtime")]
fn diagnostic_linker<T: 'static>(engine: &wasmtime::Engine) -> wasmtime::Linker<T> {
    let mut linker = wasmtime::Linker::new(engine);
    linker.func_wrap("witchy", "__witchy_abort_at",
        |mut caller: wasmtime::Caller<'_, T>, message: i32, message_len: i32,
         function: i32, function_len: i32, line: i32| -> wasmtime::Result<()> {
            let message = diagnostic_text(&mut caller, message, message_len)?;
            let function = diagnostic_text(&mut caller, function, function_len)?;
            let core = witchy_syntax::diag::DiagTemplate::Fail.render(0, 0, &message);
            wasmtime::bail!("{}", witchy_syntax::diag::runtime_error(&function, line as u32, &core));
        }).expect("bind exact canonical fail ABI");
    linker.func_wrap("witchy", "__witchy_list_bounds_at",
        |mut caller: wasmtime::Caller<'_, T>, index: i64, length: i32,
         function: i32, function_len: i32, line: i32| -> wasmtime::Result<()> {
            let function = diagnostic_text(&mut caller, function, function_len)?;
            let core = witchy_syntax::diag::DiagTemplate::ListIndexOob.render(index, i64::from(length as u32), "");
            wasmtime::bail!("{}", witchy_syntax::diag::runtime_error(&function, line as u32, &core));
        }).expect("bind exact canonical list bounds ABI");
    linker
}

#[cfg(feature = "native-test-runtime")]
fn diagnostic_text<T>(caller: &mut wasmtime::Caller<'_, T>, pointer: i32, length: i32) -> wasmtime::Result<String> {
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

fn import_names(wasm: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        if let wasmparser::Payload::ImportSection(section) = payload.expect("valid admitted Wasm") {
            for import in section.into_imports() {
                names.push(import.expect("import entry").name.to_owned());
            }
        }
    }
    names
}

#[cfg(feature = "native-test-runtime")]
fn run_oob(source: &str) -> String {
    let artifact = compile_checked_module_artifact(&checked(source))
        .expect_lowered("bounds checks permit safe canonical admission");
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, artifact.wasm()).expect("compile admitted artifact");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = diagnostic_linker(&engine).instantiate(&mut store, &module)
        .expect("instantiate admitted artifact");
    let main = instance.get_typed_func::<(), i64>(&mut store, "f0").expect("canonical main export");
    main.call(&mut store, ()).expect_err("admitted OOB access must terminate")
        .root_cause().to_string()
}

#[test]
fn admitted_artifact_carries_checked_launch_contract_and_refuses_mutation() {
    let checked = checked("fn main(console: Console):\n    console.print(\"ok\")\n");
    let expected = checked.canonical_source().expect("canonical source")
        .launch_contract_payload().to_vec();
    let artifact = compile_checked_module_artifact(&checked)
        .expect_lowered("admitted canonical artifact");

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

#[cfg(feature = "native-test-runtime")]
#[test]
fn production_entry_emits_only_admitted_canonical_bytes() {
    let checked = checked("fn main() -> Int:\n    40 + 2\n");
    let artifact = compile_checked_module_artifact(&checked)
        .expect_lowered("public production entry must preserve admission authority");
    let bytes = compile_checked_module_binary(&checked)
        .expect_lowered("public production entry must emit Wasm");
    assert_eq!(bytes, artifact.wasm());

    let mut config = wasmtime::Config::new();
    config.wasm_reference_types(true);
    config.wasm_function_references(true);
    config.wasm_gc(true);
    let engine = wasmtime::Engine::new(&config).expect("configure production Wasm engine");
    let module = wasmtime::Module::new(&engine, &bytes).expect("compile production Wasm");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = diagnostic_linker(&engine).instantiate(&mut store, &module)
        .expect("instantiate production Wasm");
    let main = instance.get_typed_func::<(), i64>(&mut store, "f0").expect("canonical main export");
    assert_eq!(main.call(&mut store, ()).expect("run production Wasm"), 42);
}

#[cfg(feature = "native-test-runtime")]
#[test]
fn list_int_out_of_bounds_source_admits_then_traps() {
    assert_eq!(run_oob("fn main() -> Int:\n    [10][1]\n"),
        "runtime error: `main`, line 2: list index 1 out of bounds (length 1)");
}

#[cfg(feature = "native-test-runtime")]
#[test]
fn list_push_keeps_length_bounds_check_for_out_of_bounds_index() {
    assert_eq!(run_oob("fn main() -> Int:\n    var xs = [1]\n    xs.push(2)\n    xs[2]\n"),
        "runtime error: `main`, line 4: list index 2 out of bounds (length 2)");
}

#[cfg(feature = "native-test-runtime")]
struct FixtureConsole;

#[cfg(feature = "native-test-runtime")]
fn fixture_console_print(
    mut caller: wasmtime::Caller<'_, Vec<u8>>,
    token: Option<wasmtime::Rooted<wasmtime::ExternRef>>,
    pointer: i32,
    length: i32,
) {
    let token = token.expect("actual fixture Console token");
    assert!(token.data(&caller).expect("live token").expect("host token").is::<FixtureConsole>());
    let memory = caller.get_export("memory")
        .and_then(wasmtime::Extern::into_memory).expect("canonical memory export");
    let start = usize::try_from(pointer).expect("non-negative print pointer");
    let length = usize::try_from(length).expect("non-negative print length");
    let bytes = memory.data(&caller)[start..start + length].to_vec();
    caller.data_mut().extend_from_slice(&bytes);
}

#[cfg(feature = "native-test-runtime")]
#[test]
fn list_string_push_moves_owned_elements_and_executes() {
    let source = "fn main(console: Console):\n    var xs = [\"first\"]\n    xs.push(\"second\")\n    console.print(xs[1])\n";
    let artifact = compile_checked_module_artifact(&checked(source))
        .expect_lowered("admit List(String) push artifact");
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, artifact.wasm()).expect("compile admitted artifact");
    let mut linker = diagnostic_linker(&engine);
    linker.func_wrap("witchy", "console_print", fixture_console_print).expect("bind authenticated Console ABI");
    let mut store = wasmtime::Store::new(&engine, Vec::<u8>::new());
    let instance = linker.instantiate(&mut store, &module).expect("instantiate admitted artifact");
    let main = instance
        .get_typed_func::<Option<wasmtime::Rooted<wasmtime::ExternRef>>, ()>(&mut store, "f0")
        .expect("canonical main export");
    let console = wasmtime::ExternRef::new(&mut store, FixtureConsole).expect("fixture Console token");
    main.call(&mut store, Some(console)).expect("run canonical List(String) push");
    assert_eq!(store.data(), b"second");
}

#[test]
fn clock_does_not_select_monotonic_or_other_capabilities() {
    let artifact = compile_checked_module_artifact(&checked("fn main(clock: Clock) -> Int:\n    clock.now()\n"))
        .expect_lowered("clock source admits");
    let imports = import_names(artifact.wasm());
    assert!(imports.iter().any(|name| name == "clock_now"), "{imports:?}");
    assert!(!imports.iter().any(|name| name == "now_monotonic"), "{imports:?}");
    assert!(witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("wrong".into(), b"fn main(console: Console) -> Int:\n    console.now()\n".to_vec())],
        "wrong", witchy_syntax::linker::ComptimeExpander::new(no_expand)).is_err(), "Console must not carry Clock authority");
}

#[test]
fn shared_callable_cannot_satisfy_unique_function_value_contract() {
    let source = "mode opt\n\nfn shared(var values: List(Int), value: Int) -> ():\n    values.push(value)\n\nfn apply(callback: fn(var unique List(Int), Int) -> (), var values: unique List(Int)) -> ():\n    callback(values, 3)\n\nfn main() -> Int:\n    var values: unique List(Int) = [1, 2]\n    apply(shared, values)\n    values.length()\n";
    let error = match witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("main".to_string(), source.as_bytes().to_vec())], "main", witchy_syntax::linker::ComptimeExpander::new(no_expand))
    {
        Ok(_) => panic!("an ordinary callable must not satisfy var unique callback contract"),
        Err(error) => error,
    };
    assert!(format!("{error:?}").contains("does not preserve Qualifier"),
        "expected the checker to reject the erased unique callback contract, got {error:?}");
}

#[test]
fn dynamic_construction_rejects_a_transitive_capability_before_lowering() {
    use witchy_types::runtime_type::{
        AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource,
    };
    let workspace = PackageCoordinate::new(PackageSource::Workspace, "example/dynamic-test", "0.1.0")
        .expect("workspace coordinate");
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/stdlib", "0.1.0")
        .expect("toolchain coordinate");
    let mut assignments = vec![(
        "main".to_string(),
        ModuleLoadIdentity::new(workspace, ["main"]).expect("main owner"),
    )];
    assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|module| {
        ((*module).to_string(),
         ModuleLoadIdentity::new(toolchain.clone(), ["std", *module]).expect("std owner"))
    }));
    let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)
        .expect("authenticated owners");
    let source = "import dynamic\nimport reflect\n\ntype Holder:\n    Holder(Console)\n\nimpl Reflect for Holder:\n    fn reflect(self) -> reflect.Mirror:\n        reflect.MNil\n\nfn main(console: Console) -> Int:\n    let value = dynamic.dynamic(Holder(console))\n    0\n";
    let error = witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        vec![("main".to_owned(), source.as_bytes().to_vec())],
        "main",
        witchy_syntax::linker::ComptimeExpander::new(no_expand),
        &std::collections::HashSet::from(["main".to_owned()]),
        owners,
    )
    .expect_err("capability-retaining Dynamic construction must be rejected");
    let message = error.to_string();
    assert!(message.contains("Console"), "{message}");
    assert!(message.contains("Holder[0]"), "{message}");
}
