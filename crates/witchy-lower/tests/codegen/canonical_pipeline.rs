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

const UNIQUE_WRITEBACK_LOOP: &str = "fn update(var values: List(Int)):\n    var i = 0\n    while i < 3:\n        values[0] = values[0] + 1\n        i = i + 1\n\nfn main() -> Int:\n    var values = [1, 2]\n    update(values)\n    values[0]\n";

#[test]
fn plain_scalar_list_writeback_keeps_its_checked_unique_entry() {
    use witchy_wir::canonical_safewir::{CallArgument, Operation, Ownership, ParameterOwnership, check_initial_with_witness};
    let input = checked(UNIQUE_WRITEBACK_LOOP);
    let candidate = witchy_lower::canonical_pipeline_checks::lowered_candidate(&input);
    let source = input.canonical_source().expect("source");
    let id = candidate.source_abi_bindings().entries().iter().find_map(|binding| {
        let declaration = source.view().closed_callable_declaration(binding.source_declaration_ordinal())?;
        (declaration.name == "main.update").then_some(binding.function())
    }).expect("update binding");
    let function = candidate.functions().iter().find(|function| function.id == id).expect("update");
    let signature = candidate.signatures().iter().find(|signature| signature.id == function.signature).expect("signature");
    assert_eq!(signature.contract.parameters[0].ownership, ParameterOwnership::UniqueWriteBack);
    assert_eq!(function.blocks[function.entry.get() as usize].parameters[0].ownership, Some(Ownership::Unique));
    assert!(!function.blocks.iter().flat_map(|block| &block.operations).any(|operation|
        matches!(operation, Operation::Alloc { .. } | Operation::CloneRange { .. })), "a unique scalar-list update copied its input");
    check_initial_with_witness(candidate.clone(), &source.view()).expect("unique writeback admits");
    let altered = candidate.clone().transform_functions(|functions| {
        let function = functions.iter_mut().find(|function| function.id == id).expect("update");
        function.blocks[function.entry.get() as usize].parameters[0].ownership = Some(Ownership::Shared);
    });
    assert!(check_initial_with_witness(altered, &source.view()).is_err(), "a shared entry cannot claim the unique physical ABI");
    let mut changed = false;
    let altered = candidate.transform_functions(|functions| {
        for function in functions {
            for block in &mut function.blocks {
                let Some(index) = block.operations.iter().position(|operation|
                    matches!(operation, Operation::Call { callee, .. } if *callee == id)) else { continue; };
                let Operation::Call { arguments, .. } = &block.operations[index] else { unreachable!() };
                let CallArgument::Move(value) = arguments[0] else { panic!("unique input must move") };
                let shared = witchy_wir::canonical_safewir::ValueId::new(
                    function.value_types.keys().max().expect("values").get().checked_add(1).expect("fresh value"));
                function.value_types.insert(shared, function.value_types[&value]);
                block.operations.insert(index, Operation::CloneRef { destination: shared, source: value });
                let Operation::Call { arguments, .. } = &mut block.operations[index + 1] else { unreachable!() };
                arguments[0] = CallArgument::Move(shared);
                changed = true;
            }
        }
    });
    assert!(changed, "the mutation must replace a real unique call argument");
    assert!(check_initial_with_witness(altered, &source.view()).is_err(), "a shared caller token cannot acquire unique argument authority");
}

#[cfg(feature = "native-test-runtime")]
#[test]
fn plain_scalar_list_writeback_preserves_snapshots_and_indirect_calls() {
    use witchy_syntax::opt::{Opt, OptSet};
    let _reset = ResetCallbackOptimization;
    for options in [OptSet::none(), OptSet::none().with(Opt::ClosureElide)] {
        witchy_syntax::opt::set_for_tests(Some(options));
        assert_eq!(run_callback_with_heap_audit(UNIQUE_WRITEBACK_LOOP, true).expect("writeback result"), 4);
        for (source, expected) in [
            ("fn update(var values: List(Int)):\n    values[0] = 9\n\nfn main() -> Int:\n    var values = [1]\n    let before = values\n    update(values)\n    before[0] * 10 + values[0]\n", 19),
            ("fn update(var values: List(Int)) -> Int:\n    let before = values\n    values[0] = 9\n    before[0]\n\nfn main() -> Int:\n    var values = [1]\n    let before = update(values)\n    before * 10 + values[0]\n", 19),
            ("fn update(var values: List(Int)):\n    values[0] = 9\n\nfn apply(f: fn(var List(Int)) -> (), var values: List(Int)):\n    f(values)\n\nfn main() -> Int:\n    var values = [1]\n    let before = values\n    apply(update, values)\n    before[0] * 10 + values[0]\n", 19),
            ("fn replace(var values: List(Int), let other: List(Int)):\n    values = other\n\nfn main() -> Int:\n    var values = [1]\n    let other = [7]\n    replace(values, other)\n    values[0] = 9\n    other[0] * 10 + values[0]\n", 79),
            ("fn replace(var values: List(Int), other: List(Int)):\n    values = other\n\nfn main() -> Int:\n    var values = [1]\n    let other = [7]\n    replace(values, other)\n    values[0] = 9\n    other[0] * 10 + values[0]\n", 79),
            ("type Bag:\n    values: List(Int)\n\nfn update(var values: List(Int)):\n    values[0] = 9\n\nfn main() -> Int:\n    var bag = Bag([1])\n    let before = bag\n    update(bag.values)\n    before.values[0] * 10 + bag.values[0]\n", 19),
            ("fn digit(var n: Int, value: Int) -> Int:\n    n = n * 10 + value\n    value\n\nfn update(var values: List(Int), first: Int, second: Int):\n    values[0] = first * 10 + second\n\nfn main() -> Int:\n    var values = [0]\n    var order = 0\n    update(values, digit(order, 1), digit(order, 2))\n    order * 100 + values[0]\n", 1212),
            ("fn update(var values: List(Bool)):\n    values[0] = false\n\nfn main() -> Int:\n    var values = [true]\n    let before = values\n    update(values)\n    if before[0] && !values[0]: 1 else: 0\n", 1),
            ("fn update(var values: List(Float)):\n    values[0] = 2.5\n\nfn main() -> Int:\n    var values = [1.5]\n    let before = values\n    update(values)\n    if before[0] == 1.5 && values[0] == 2.5: 1 else: 0\n", 1),
        ] {
            assert_eq!(run_callback_with_heap_audit(source, true).expect("writeback result"), expected);
        }
    }
}

#[test]
fn opaque_sum_match_authenticates_tag_once_across_alternatives() {
    use witchy_wir::canonical_safewir::{Operation, closure_owner_contract::OwnerOperation};
    let input = checked("type Choice:\n    A(Console)\n    B(Console)\n    C(Console)\n    D(Console)\n\nfn classify(value: Choice) -> Int:\n    match value:\n        A(_) -> 1\n        B(_) -> 2\n        C(_) -> 3\n        D(_) -> 4\n\nfn main(console: Console) -> Int:\n    classify(D(console))\n");
    let candidate = witchy_lower::canonical_pipeline_checks::lowered_candidate(&input);
    let source = input.canonical_source().expect("source");
    let id = candidate.source_abi_bindings().entries().iter().find_map(|binding| {
        let declaration = source.view().closed_callable_declaration(binding.source_declaration_ordinal())?;
        (declaration.name == "main.classify").then_some(binding.function())
    }).expect("classify binding");
    let function = candidate.functions().iter().find(|function| function.id == id).expect("classify");
    let reads = function.blocks.iter().flat_map(|block| &block.operations).filter(|operation|
        matches!(operation, Operation::ClosureOwner(OwnerOperation::ReadTag { .. }))).count();
    assert_eq!(reads, 1);
    compile_checked_module_artifact(&input).expect_lowered("admit reused owner tag");
}

#[test]
fn opaque_sum_tag_facts_keep_branch_and_loop_dominance() {
    let declarations = "type Choice:\n    A(Console)\n    B(Console)\n\nfn classify(value: Choice) -> Int:\n    match value:\n        A(_) -> 1\n        B(_) -> 2\n\n";
    for body in [
        "fn main(console: Console) -> Int:\n    let value = B(console)\n    let first = if true:\n        match value:\n            A(_) -> 1\n            B(_) -> 2\n    else: 0\n    let second = match value:\n        A(_) -> 3\n        B(_) -> 4\n    first + second\n",
        "fn main(console: Console) -> Int:\n    var value = A(console)\n    var count = 0\n    while count < 3:\n        let increment = match value:\n            A(_) -> 1\n            B(_) -> 2\n        count = count + increment\n        value = B(console)\n    let last = match value:\n        A(_) -> 3\n        B(_) -> 4\n    last + count\n",
        "fn main(console: Console) -> Int:\n    let value = B(console)\n    match value:\n        A(_) -> match value:\n            A(_) -> 1\n            B(_) -> 2\n        B(_) -> match value:\n            A(_) -> 3\n            B(_) -> 4\n",
    ] {
        let input = checked(&format!("{declarations}{body}"));
        compile_checked_module_artifact(&input).expect_lowered("admit branch-local tag facts");
    }
}

struct ResetCallbackOptimization;

impl Drop for ResetCallbackOptimization {
    fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); }
}

fn callback_structure(source_text: &str, enabled: bool) -> (usize, usize) {
    use witchy_syntax::opt::{Opt, OptSet};
    use witchy_wir::canonical_safewir::Operation;
    let _reset = ResetCallbackOptimization;
    let options = if enabled { OptSet::none().with(Opt::ClosureElide) } else { OptSet::none() };
    witchy_syntax::opt::set_for_tests(Some(options));
    let checked = checked(source_text);
    let candidate = witchy_lower::canonical_pipeline_checks::lowered_candidate(&checked);
    let source = checked.canonical_source().expect("source");
    let main = candidate.source_abi_bindings().entries().iter().find_map(|binding| {
        let declaration = source.view().closed_callable_declaration(binding.source_declaration_ordinal())?;
        (declaration.name == "main").then_some(binding.function())
    }).expect("main binding");
    let main = candidate.functions().iter().find(|function| function.id == main).expect("main");
    let operations = main.blocks.iter().flat_map(|block| &block.operations).collect::<Vec<_>>();
    let allocations = operations.iter().filter(|operation| matches!(operation, Operation::Alloc { .. })).count();
    let clones = operations.iter().filter(|operation| matches!(operation, Operation::CloneRef { .. })).count();
    if source_text == SCALAR_CALLBACK {
        let expected_name = if enabled { "main.twice" } else { "main.invoke" };
        let expected = candidate.source_abi_bindings().entries().iter().find_map(|binding| {
            let declaration = source.view().closed_callable_declaration(binding.source_declaration_ordinal())?;
            (declaration.name == expected_name).then_some(binding.function())
        }).expect("checked target binding");
        let callees = operations.iter().filter_map(|operation| match operation {
            Operation::Call { callee, .. } => Some(*callee), _ => None,
        }).collect::<Vec<_>>();
        assert_eq!(callees, [expected]);
    }
    compile_checked_module_artifact(&checked).expect_lowered("admitted callback forwarder");
    (allocations, clones)
}

const SCALAR_CALLBACK: &str = "fn twice(x: Int) -> Int:\n    x * 2\n\nfn invoke(f: fn(Int) -> Int, x: Int) -> Int:\n    f(x)\n\nfn main() -> Int:\n    let value = invoke(twice, 21)\n    value + 1\n";

#[test]
fn scalar_callback_forwarder_structure() {
    assert_eq!(callback_structure(SCALAR_CALLBACK, false), (2, 1));
    assert_eq!(callback_structure(SCALAR_CALLBACK, true), (0, 0));
}

#[test]
fn scalar_callback_forwarder_keeps_unsupported_routes() {
    for source in [
        SCALAR_CALLBACK.replace("    f(x)", "    f(x) + 1"),
        SCALAR_CALLBACK.replace("    f(x)", "    f(x) + f(x)"),
        SCALAR_CALLBACK.replace("    x * 2", "    twice(x - 1)"),
        SCALAR_CALLBACK.replace("    f(x)", "    let alias = f\n    alias(x)"),
        SCALAR_CALLBACK.replace("invoke(twice, 21)", "invoke(fn(x: Int): x * 2, 21)"),
        SCALAR_CALLBACK.replace("    let value = invoke(twice, 21)", "    let callback = twice\n    let value = invoke(callback, 21)"),
        SCALAR_CALLBACK.replace("    let value = invoke(twice, 21)", "    let twice = fn(x: Int): x + 3\n    let value = invoke(twice, 21)"),
        SCALAR_CALLBACK.replace("    let value = invoke(twice, 21)", "    let invoke = fn(f: fn(Int) -> Int, x: Int): f(x)\n    let value = invoke(twice, 21)"),
        SCALAR_CALLBACK.replace("    let value = invoke(twice, 21)\n    value + 1", "    invoke(twice, 21)"),
    ] {
        assert_eq!(callback_structure(&source, true), callback_structure(&source, false), "{source}");
    }
}

#[cfg(feature = "native-test-runtime")]
#[test]
fn scalar_callback_forwarder_runs_admitted_optimized_and_unoptimized_wasm() {
    use witchy_syntax::opt::{Opt, OptSet};
    let _reset = ResetCallbackOptimization;
    for options in [OptSet::none(), OptSet::none().with(Opt::ClosureElide)] {
        witchy_syntax::opt::set_for_tests(Some(options));
        assert_eq!(run_callback_result(SCALAR_CALLBACK), 43);
        let ordered = "fn difference(a: Int, b: Int) -> Int:\n    a - b\n\nfn invoke(f: fn(Int, Int) -> Int, a: Int, b: Int) -> Int:\n    f(a, b)\n\nfn next(var n: Int, digit: Int) -> Int:\n    n = n * 10 + digit\n    digit\n\nfn main() -> Int:\n    var n = 0\n    let result = invoke(difference, next(n, 1), next(n, 2))\n    n * 10 + result\n";
        assert_eq!(run_callback_result(ordered), 119);
        for source in [
            "fn constant() -> Int:\n    42\n\nfn invoke(f: fn() -> Int) -> Int:\n    f()\n\nfn main() -> Int:\n    let result = invoke(constant)\n    result + 1\n",
            "fn invert(x: Bool) -> Bool:\n    !x\n\nfn invoke(f: fn(Bool) -> Bool, x: Bool) -> Bool:\n    f(x)\n\nfn main() -> Int:\n    let result = invoke(invert, false)\n    if result: 43 else: 0\n",
            "fn twice(x: Float) -> Float:\n    x * 2.0\n\nfn invoke(f: fn(Float) -> Float, x: Float) -> Float:\n    f(x)\n\nfn main() -> Int:\n    let result = invoke(twice, 21.0)\n    if result == 42.0: 43 else: 0\n",
        ] {
            assert_eq!(run_callback_result(source), 43);
        }
    }
}

#[cfg(feature = "native-test-runtime")]
fn run_callback_result(source_text: &str) -> i64 {
    run_callback(source_text).expect("callback result")
}

#[cfg(feature = "native-test-runtime")]
pub(super) fn run_callback(source_text: &str) -> Result<i64, String> {
    run_callback_with_heap_audit(source_text, false)
}

#[cfg(feature = "native-test-runtime")]
fn run_callback_with_heap_audit(source_text: &str, audit: bool) -> Result<i64, String> {
    let checked = checked(source_text);
    let candidate = witchy_lower::canonical_pipeline_checks::lowered_candidate(&checked);
    let source = checked.canonical_source().expect("source");
    let main = candidate.source_abi_bindings().entries().iter().find_map(|binding| {
        let declaration = source.view().closed_callable_declaration(binding.source_declaration_ordinal())?;
        (declaration.name == "main").then_some(binding.function())
    }).expect("main binding");
    let artifact = compile_checked_module_artifact(&checked).expect_lowered("admit callback source");
    let mut config = wasmtime::Config::new();
    config.compiler_inlining(wasmtime::Inlining::Yes);
    let engine = wasmtime::Engine::new(&config).expect("optimized engine");
    let module = wasmtime::Module::new(&engine, artifact.wasm()).expect("compile admitted callback");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = diagnostic_linker(&engine).instantiate(&mut store, &module).expect("instantiate callback");
    let result = instance.get_typed_func::<(), i64>(&mut store, &format!("f{}", main.get()))
        .expect("callback main export").call(&mut store, ()).map_err(|error| error.root_cause().to_string())?;
    if audit {
        let mut global = |name| instance.get_global(&mut store, name).expect("heap global")
            .get(&mut store).i32().expect("heap address") as usize;
        let (top, mut free, mut cursor) = (global(witchy_wir::HEAP_TOP_EXPORT),
            global(witchy_wir::HEAP_FREE_EXPORT), global(witchy_wir::HEAP_BASE_EXPORT));
        let memory = instance.get_memory(&mut store, "memory").expect("memory");
        let word = |at: usize| u32::from_le_bytes(memory.data(&store)[at..at + 4].try_into().expect("heap word")) as usize;
        let mut freed = std::collections::BTreeSet::new();
        while free != 0 {
            assert!(freed.insert(free), "free-list cycle or double release");
            free = word(free);
        }
        while cursor + 8 <= top {
            let size = word(cursor + 4);
            assert!(size > 0, "allocation must have a nonempty header");
            assert!(freed.contains(&(cursor + 8)), "allocation survived writeback completion");
            cursor += 8 + size;
        }
        assert_eq!(cursor, top, "the audit visits the complete allocated heap");
    }
    Ok(result)
}

#[cfg(feature = "native-test-runtime")]
#[test]
fn singleton_match_projection_preserves_snapshots_and_bounds_diagnostics() {
    use witchy_syntax::opt::OptSet;
    let _reset = ResetCallbackOptimization;
    let scalar = "fn select(flag: Bool) -> Int:\n    (match flag:\n        true -> [42]\n        false -> []\n    )[0]\n\nfn main() -> Int:\n    select(true)\n";
    let snapshot = "fn select(flag: Bool, value: List(Int)) -> List(Int):\n    (match flag:\n        true -> [value]\n        false -> []\n    )[0]\n\nfn main() -> Int:\n    let original = [1, 2]\n    var selected = select(true, original)\n    selected.set_at(0, 9)\n    original.at(0)\n";
    let callable = "fn select(flag: Bool) -> fn(Int) -> Int:\n    (match flag:\n        true -> [fn(n: Int): n + 1]\n        false -> []\n    )[0]\n\nfn main() -> Int:\n    let selected = select(true)\n    selected(7)\n";
    let failed = scalar.replace("select(true)", "select(false)");
    let expected = witchy_syntax::diag::runtime_error("main.select", 2,
        &witchy_syntax::diag::DiagTemplate::ListIndexOob.render(0, 0, ""));
    for optimization in [OptSet::none(), OptSet::all()] {
        witchy_syntax::opt::set_for_tests(Some(optimization));
        assert_eq!(run_callback_result(scalar), 42);
        assert_eq!(run_callback_result(snapshot), 1);
        assert_eq!(run_callback_result(callable), 8);
        assert_eq!(run_callback(&failed).expect_err("empty projection traps"), expected);
    }
}

#[cfg(feature = "native-test-runtime")]
#[test]
fn scalar_callback_forwarder_preserves_traps() {
    use witchy_syntax::opt::{Opt, OptSet};
    let _reset = ResetCallbackOptimization;
    for source in [
        SCALAR_CALLBACK.replace("x * 2", "x / (x - 21)"),
        SCALAR_CALLBACK.replace("x * 2", "x % (x - 21)"),
        SCALAR_CALLBACK.replace("21)", "(-9223372036854775807 - 1))").replace("x * 2", "x / -1"),
        "fn difference(a: Int, b: Int) -> Int:\n    a - b\n\nfn invoke(f: fn(Int, Int) -> Int, a: Int, b: Int) -> Int:\n    f(a, b)\n\nfn first() -> Int:\n    fail(\"first argument\")\n    0\n\nfn second() -> Int:\n    fail(\"second argument\")\n    0\n\nfn main() -> Int:\n    let result = invoke(difference, first(), second())\n    result + 1\n".to_owned(),
    ] {
        let source = source.replace("fn main() -> Int:\n", "fn main() -> Int:\n    let marker: String = \"trap\"\n    let _ = marker\n");
        witchy_syntax::opt::set_for_tests(Some(OptSet::none()));
        let baseline = run_callback(&source).expect_err("unoptimized trap");
        witchy_syntax::opt::set_for_tests(Some(OptSet::none().with(Opt::ClosureElide)));
        let optimized = run_callback(&source).expect_err("optimized trap");
        assert_eq!(optimized, baseline, "{source}");
        if source.contains("first argument") { assert!(optimized.contains("first argument"), "{optimized}"); }
    }
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

#[cfg(feature = "native-test-runtime")]
fn run_index_result(source: &str, export: &str) -> i64 {
    let artifact = compile_checked_module_artifact(&checked(source))
        .expect_lowered("admit source bounds proof");
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, artifact.wasm()).expect("compile admitted artifact");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = diagnostic_linker(&engine).instantiate(&mut store, &module)
        .expect("instantiate admitted artifact");
    instance.get_typed_func::<(), i64>(&mut store, export)
        .expect("canonical main export").call(&mut store, ()).expect("bounded read")
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

#[test]
fn only_proven_branches_fold_before_the_check() {
    let lowered = witchy_lower::canonical_pipeline_checks::lowered_candidate(&checked(
        "import bytes\n\nfn main() -> Int:\n    let b = bytes.from_string(\"abc\")\n    var i = 0\n    var total = 0\n    while i < b.length():\n        total = total + b.at(i)\n        i = i + 1\n    total\n"));
    witchy_wir::self_checks::canonical_safewir_optimize::folded_branches_must_be_implied(&lowered);
}

#[test]
fn an_int_renders_one_digit_per_written_byte() {
    // Digits are written by a loop over the rendered length, not by 19
    // unrolled writes whose slots each cost a division by the digit count.
    use witchy_wir::canonical_safewir::{ArithmeticOperator, Operation};
    let lowered = witchy_lower::canonical_pipeline_checks::lowered_candidate(&checked(
        "fn main() -> Int:\n    let n = 42\n    \"${n}\".length()\n"));
    let operations = lowered.functions().iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.operations)
        .collect::<Vec<_>>();
    let copies = operations.iter().filter(|operation| matches!(operation, Operation::CopyBytes { .. })).count();
    // The unrolled form copied one byte for each of 19 decimal places.
    assert!(copies < 19, "rendering one Int copies {copies} byte ranges");
    assert!(!operations.iter().any(|operation| matches!(operation,
        Operation::Arithmetic { operator: ArithmeticOperator::RemainderUnsigned, .. })),
        "digit slots are computed by a division");
}

#[test]
fn an_invariant_string_is_not_reauthenticated_every_iteration() {
    use witchy_wir::canonical_safewir::{Operation, Terminator};
    let lowered = witchy_lower::canonical_pipeline_checks::lowered_candidate(&checked(
        "import bytes\n\nfn table() -> Bytes:\n    bytes.from_string(\"abcd\")\n\nfn main() -> Int:\n    let t = table()\n    var i = 0\n    var total = 0\n    while i < 1000:\n        total = total + t.at(i % 4)\n        i = i + 1\n    total\n"));
    let successors = |terminator: &Terminator| match terminator {
        Terminator::Branch { target, .. } => vec![*target],
        Terminator::BranchIf { then_target, else_target, .. } => vec![*then_target, *else_target],
        _ => Vec::new(),
    };
    for function in lowered.functions() {
        let edges: std::collections::BTreeMap<_, _> = function.blocks.iter()
            .map(|block| (block.id, successors(&block.terminator))).collect();
        // A block on a cycle reaches itself.
        let cyclic = |start| {
            let mut seen = std::collections::BTreeSet::new();
            let mut pending = edges[&start].clone();
            while let Some(block) = pending.pop() {
                if block == start { return true; }
                if seen.insert(block) { pending.extend(edges[&block].iter().copied()); }
            }
            false
        };
        for block in &function.blocks {
            let reauthenticates = block.operations.iter()
                .any(|operation| matches!(operation, Operation::ReauthenticateRawHeaderExtent { .. }));
            assert!(!(reauthenticates && cyclic(block.id)),
                "{:?} reloads an unchanged header inside a loop at {:?}", function.id, block.id);
        }
    }
}

const INVARIANT_TABLE: &str = "import bytes\n\nfn table() -> Bytes:\n    bytes.from_string(\"abcd\")\n\n";

#[test]
fn an_invariant_string_survives_an_if_whose_other_arm_returns() {
    let source = format!("{INVARIANT_TABLE}fn main() -> Int:\n    let t = table()\n    var i = 0\n    var total = 0\n    while i < 10:\n        total = total + t.at(i % 4)\n        if i == 7:\n            return total\n        else:\n            var j = 0\n            while j < 2:\n                total = total + t.at(j)\n                j = j + 1\n        i = i + 1\n    total\n");
    compile_checked_module_artifact(&checked(&source))
        .expect_lowered("lanes an inner loop renamed in the continuing arm are not reinstated");
}

#[test]
fn an_invariant_string_survives_a_match_arm_with_an_inner_loop() {
    let source = format!("{INVARIANT_TABLE}fn main() -> Int:\n    let t = table()\n    var i = 0\n    var total = 0\n    while i < 20:\n        total = total + t.at(i % 4)\n        match i % 5:\n            0 ->\n                var j = 0\n                while j < 3:\n                    total = total + t.at(j)\n                    j = j + 1\n            _ -> total = total + 1\n        i = i + 1\n    total\n");
    compile_checked_module_artifact(&checked(&source))
        .expect_lowered("a match join drops lanes one arm renamed");
}

#[test]
fn an_invariant_string_survives_a_counted_for_with_an_inner_loop() {
    let source = format!("{INVARIANT_TABLE}fn main() -> Int:\n    let t = table()\n    var total = 0\n    for k in 0..5:\n        var j = 0\n        while j < 4:\n            total = total + t.at(j)\n            j = j + 1\n    total\n");
    compile_checked_module_artifact(&checked(&source))
        .expect_lowered("a counted for carries a String an inner loop renames");
}

#[test]
fn an_invariant_string_survives_a_quadratic_scan() {
    let source = format!("{INVARIANT_TABLE}fn count(t: Bytes) -> Int:\n    var total = 0\n    for k in 0..t.length():\n        var j = k\n        while j < t.length():\n            if t.at(j) == t.at(k):\n                total = total + 1\n            j = j + 1\n    total\n\nfn main() -> Int:\n    count(table())\n");
    compile_checked_module_artifact(&checked(&source))
        .expect_lowered("a parameter String scanned by nested loops keeps a checked extent");
}

const STEP: &str = "fn step(i: Int) -> Option(Int):\n    if i < 3: Some(i) else: None\n\n";

#[test]
fn an_invariant_string_survives_a_while_let_with_an_inner_loop() {
    let source = format!("{INVARIANT_TABLE}{STEP}fn sum(t: Bytes) -> Int:\n    var total = 0\n    var i = 0\n    while let Some(x) = step(i):\n        var j = 0\n        while j < 4:\n            total = total + t.at(j)\n            j = j + 1\n        i = i + 1\n    total\n\nfn main() -> Int:\n    sum(table())\n");
    compile_checked_module_artifact(&checked(&source))
        .expect_lowered("a while let carries an invariant String's lanes past an inner while");
}

#[test]
fn an_invariant_string_survives_a_while_let_with_an_inner_counted_for() {
    let source = format!("{INVARIANT_TABLE}{STEP}fn sum(t: Bytes) -> Int:\n    var total = 0\n    var i = 0\n    while let Some(x) = step(i):\n        for y in 0..2:\n            total = total + t.at(y + x % 2)\n        i = i + 1\n    total\n\nfn main() -> Int:\n    sum(table())\n");
    compile_checked_module_artifact(&checked(&source))
        .expect_lowered("a while let carries an invariant String's lanes past an inner counted for");
}

#[test]
fn a_static_string_survives_a_counted_for_that_never_mentions_it() {
    // The inner `for` reauthenticates the literal at its header; the outer
    // backedge's fresh lanes must meet the entry's, which static data allows
    // only in the same count-for-capacity form.
    let source = "fn main() -> Int:\n    let w = \"wxyz\"\n    var acc = 0\n    for i in 0..3:\n        var q = 0\n        while q < w.length():\n            acc = acc + w.substring(q, w.length()).length()\n            q = q + 1\n        for k in 0..2:\n            acc = acc + k\n    acc\n";
    compile_checked_module_artifact(&checked(source))
        .expect_lowered("a literal String carried by an outer for keeps a checked extent");
}

#[test]
fn an_inner_loop_carries_a_string_only_when_its_enclosing_loop_does() {
    let source = "import bytes\n\nfn main() -> Int:\n    let b = bytes.from_string(\"abcdefgh\")\n    var acc = 0\n    var i = 0\n    while i < 5:\n        var q = 0\n        while q < b.length():\n            acc = acc + b.at(q)\n            q = q + 1\n        for r in 0..3:\n            acc = acc + b.at(r)\n        i = i + 1\n    acc % 256\n";
    compile_checked_module_artifact(&checked(source))
        .expect_lowered("an inner for does not rename lanes its enclosing while keeps at their pre-loop ids");
}

#[test]
fn a_while_refreshes_a_string_its_enclosing_loop_does_not_carry() {
    let source = "import bytes\n\nfn main() -> Int:\n    let b = bytes.from_string(\"abcdefgh\")\n    var acc = 0\n    var i = 0\n    while i < 3:\n        for k in 0..b.length():\n            var q = 0\n            while q < 3:\n                acc = acc + b.at(k)\n                q = q + 1\n        acc = acc + b.at(1)\n        i = i + 1\n    acc % 256\n";
    compile_checked_module_artifact(&checked(source))
        .expect_lowered("an inner while under an uncarrying loop reauthenticates its String at every edge");
}

#[test]
fn an_alias_bound_in_a_loop_does_not_carry_its_source_string_exactly() {
    // `c` is a fresh reference to `b`'s extent; the outer loop carries
    // neither, so the inner loop must not rename that extent's lanes.
    let source = "fn go(n: Int) -> Int:\n    let b = \"abcdefg\" + \"h\"\n    var acc = 0\n    for i in 0..n:\n        let c = b\n        var q = 0\n        while q < 3:\n            q = q + 1\n            acc = acc + c.substring(q, q + 1).length()\n    acc\n\nfn main() -> Int:\n    go(3)\n";
    compile_checked_module_artifact(&checked(source))
        .expect_lowered("an inner loop under a loop that does not carry the extent refreshes the alias");
}

#[test]
fn two_aliases_of_one_string_enter_a_loop_on_their_own_lanes() {
    // `b` and `c` hold the same lanes; carried on one entry id, the
    // checker would count both extents by `b`'s header parameter.
    let source = "fn go(s0: String) -> Int:\n    let b = s0\n    let c = s0\n    var acc = 0\n    var p = 0\n    while p < 3:\n        p = p + 1\n        var q = 0\n        while q < 3:\n            q = q + 1\n            acc = acc + c.substring(1, 2).length()\n        acc = acc + b.substring(1, 2).length()\n    acc\n\nfn main() -> Int:\n    go(\"abcdefgh\") % 256\n";
    compile_checked_module_artifact(&checked(source))
        .expect_lowered("aliased Strings carried through nested loops keep checked extents");
}

const SWAP_LOOP: &str = "fn flip(var xs: List(Int), k: Int):\n    var lo = 0\n    var hi = k\n    while lo < hi:\n        let a = list.at(xs, lo)\n        let b = list.at(xs, hi)\n        list.set_at(xs, lo, b)\n        list.set_at(xs, hi, a)\n        lo = lo + 1\n        hi = hi - 1\n\nfn main() -> Int:\n    var xs = [1, 2, 3, 4, 5]\n    flip(xs, 4)\n    list.at(xs, 0)\n";

#[test]
fn a_write_below_the_length_needs_no_capacity_check() {
    let checked = checked(SWAP_LOOP);
    let lowered = witchy_lower::canonical_pipeline_checks::lowered_candidate(&checked);
    let (rewritten, _) = witchy_wir::self_checks::canonical_safewir_optimize::strict_bound_through_length(&lowered);
    let source = checked.canonical_source().expect("canonical source");
    witchy_wir::canonical_safewir::check_initial_with_witness(rewritten, &source.view())
        .expect("index < length <= capacity bounds the write");
}

#[test]
fn a_write_bound_through_the_length_needs_the_length_capacity_link() {
    let checked = checked(SWAP_LOOP);
    let lowered = witchy_lower::canonical_pipeline_checks::lowered_candidate(&checked);
    let (_, forged) = witchy_wir::self_checks::canonical_safewir_optimize::strict_bound_through_length(&lowered);
    let source = checked.canonical_source().expect("canonical source");
    assert!(witchy_wir::canonical_safewir::check_initial_with_witness(forged, &source.view()).is_err(),
        "without length <= capacity, index < length does not bound the write");
}

#[test]
fn exact_narrowing_needs_a_proven_bound() {
    // `get`'s index is bounded only by `bytes.at`'s own check.
    let checked = checked("import bytes\n\nfn get(b: Bytes, i: Int) -> Int:\n    b.at(i)\n\nfn main() -> Int:\n    get(bytes.from_string(\"abc\"), 1)\n");
    let lowered = witchy_lower::canonical_pipeline_checks::lowered_candidate(&checked);
    let forged = witchy_wir::self_checks::canonical_safewir_optimize::unproven_exact_narrowing(&lowered);
    let source = checked.canonical_source().expect("canonical source");
    let error = witchy_wir::canonical_safewir::check_initial_with_witness(forged, &source.view())
        .err().expect("an exact narrowing without a proven bound is refused");
    assert!(format!("{error:?}").contains("NarrowSize exact without a proven bound"), "{error:?}");
}

#[test]
fn trapping_check_arms_are_hinted_cold_at_their_branches() {
    let checked = checked("fn main() -> Int:\n    [10][1]\n");
    let artifact = compile_checked_module_artifact(&checked)
        .expect_lowered("bounds checks permit safe canonical admission");
    let mut imported = 0;
    let mut hints = Vec::new();
    let mut branches = std::collections::BTreeMap::new();
    let mut function = 0;
    for payload in wasmparser::Parser::new(0).parse_all(artifact.wasm()) {
        match payload.expect("valid admitted Wasm") {
            wasmparser::Payload::ImportSection(section) => imported = section.into_imports().count() as u32,
            wasmparser::Payload::CustomSection(section) => {
                if let wasmparser::KnownCustom::BranchHints(reader) = section.as_known() {
                    for entry in reader {
                        let entry = entry.expect("branch hint function");
                        for hint in entry.hints {
                            let hint = hint.expect("branch hint");
                            hints.push((entry.func, hint.func_offset, hint.taken));
                        }
                    }
                }
            }
            wasmparser::Payload::CodeSectionEntry(body) => {
                let start = body.range().start;
                let mut operators = body.get_operators_reader().expect("operators");
                while !operators.eof() {
                    let (operator, offset) = operators.read_with_offset().expect("operator");
                    let kind = match operator {
                        wasmparser::Operator::If { .. } => "if",
                        wasmparser::Operator::BrIf { .. } => "br_if",
                        _ => continue,
                    };
                    branches.insert((imported + function, (offset - start) as u32), kind);
                }
                function += 1;
            }
            _ => {}
        }
    }
    assert!(hints.iter().any(|(func, offset, taken)| !taken && branches.get(&(*func, *offset)) == Some(&"br_if")),
        "the failed bounds check branches out of line with a not-taken hint: {hints:?}");
    for (func, offset, _) in &hints {
        assert!(branches.contains_key(&(*func, *offset)), "hint at {func}:{offset} names no branch");
    }
}

#[cfg(feature = "native-test-runtime")]
#[test]
fn list_int_out_of_bounds_source_admits_then_traps() {
    assert_eq!(run_oob("fn main() -> Int:\n    [10][1]\n"),
        "runtime error: `main`, line 2: list index 1 out of bounds (length 1)");
}

#[cfg(feature = "native-test-runtime")]
#[test]
fn reference_facts_preserve_snapshots_across_scalar_stores_and_loop_joins() {
    struct ResetOptimization;
    impl Drop for ResetOptimization {
        fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); }
    }
    let _reset = ResetOptimization;
    let fixtures = [
        ("fn main() -> Int:\n    var xs = [11, 22]\n    xs[0] = 17\n    let before = xs\n    var i = 0\n    while i < 3:\n        xs[i % 2] = i + 50\n        i = i + 1\n    let after = xs\n    xs[0] = 90\n    before[0] + before[1] + after[0] + after[1] + xs[0] + xs[1]\n", 283),
        ("fn main() -> Int:\n    var xs = [10, 20]\n    var turn = 0\n    var sum = 0\n    while turn < 3:\n        let snapshot = xs\n        if turn % 2 == 0:\n            xs[0] = xs[0] + turn + 1\n        else:\n            xs[1] = xs[1] + turn + 1\n        sum = sum + snapshot[0] + snapshot[1]\n        turn = turn + 1\n    sum + xs[0] + xs[1]\n", 130),
    ];
    for optimization in [witchy_syntax::opt::OptSet::none(), witchy_syntax::opt::OptSet::all()] {
        witchy_syntax::opt::set_for_tests(Some(optimization));
        for (source, expected) in fixtures {
            assert_eq!(run_index_result(source, "f0"), expected);
        }
    }
}

#[cfg(feature = "native-test-runtime")]
#[test]
fn list_push_keeps_length_bounds_check_for_out_of_bounds_index() {
    assert_eq!(run_oob("fn main() -> Int:\n    var xs = [1]\n    xs.push(2)\n    xs[2]\n"),
        "runtime error: `main`, line 4: list index 2 out of bounds (length 2)");
}

#[cfg(feature = "native-test-runtime")]
#[test]
fn branch_bounds_prove_if_and_while_reads() {
    let if_source = "mode opt\nfn read(own xs: List(Int), let i: Int, let ready: Bool) -> Int:\n    if ready && i >= 0 && i < list.length(xs):\n        xs[i] + xs[i]\n    else:\n        0\nfn main() -> Int:\n    read([10, 20, 30], 1, true)\n";
    let while_source = "mode opt\nfn main() -> Int:\n    let xs = [10, 20, 30]\n    var i = 0\n    var total = 0\n    while i >= 0 && i < list.length(xs):\n        total += xs[i]\n        i += 1\n    total\n";
    assert_eq!(run_index_result(if_source, "f1"), 40);
    assert_eq!(run_index_result(while_source, "f0"), 60);
}

#[cfg(feature = "native-test-runtime")]
#[test]
fn branch_bounds_do_not_authorize_mutated_or_unproven_indices() {
    let rebound = "mode opt\nfn main() -> Int:\n    var xs = [10, 20]\n    let i = 1\n    if i >= 0 && i < list.length(xs):\n        xs = []\n        xs[i]\n    else:\n        0\n";
    let advanced = "mode opt\nfn main() -> Int:\n    let xs = [10, 20]\n    var i = 1\n    while i >= 0 && i < list.length(xs):\n        i += 1\n        xs[i]\n    0\n";
    let disjunction = "mode opt\nfn main() -> Int:\n    let xs = [10, 20]\n    let i = -1\n    if i >= 0 || i < list.length(xs):\n        xs[i]\n    else:\n        0\n";
    assert_eq!(run_oob(rebound), "runtime error: `main`, line 7: list index 1 out of bounds (length 0)");
    assert_eq!(run_oob(advanced), "runtime error: `main`, line 7: list index 2 out of bounds (length 2)");
    assert_eq!(run_oob(disjunction), "runtime error: `main`, line 6: list index -1 out of bounds (length 2)");
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
    let source = "mode opt\n\nfn shared(var values: List(Int), value: Int) -> ():\n    values = [value]\n\nfn apply(callback: fn(var unique List(Int), Int) -> (), var values: unique List(Int)) -> ():\n    callback(values, 3)\n\nfn main() -> Int:\n    var values: unique List(Int) = [1, 2]\n    apply(shared, values)\n    values.length()\n";
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

const LENT_DICT: &str = "fn keep(let d: Dict(String, Int), var out: List(Dict(String, Int))):\n    out.push(d)\n\nfn size_of(let d: Dict(String, Int)) -> Int:\n    d.length()\n\nfn main() -> Int:\n    var d: Dict(String, Int) = dict.with_capacity(4)\n    var kept: List(Dict(String, Int)) = []\n    var i = 0\n    while i < 100:\n        let n = d.get(\"a\") ?? 0\n        let _ = d.insert(\"a\", n + size_of(d))\n        i = i + 1\n    keep(d, kept)\n    d.get_or(\"a\", 0) + kept.length()\n";

#[test]
fn a_let_borrowed_dictionary_is_read_without_a_copy() {
    // `get` and `size_of` cannot keep their `let` receiver, so the loop lends
    // the unique table instead of copying it before every call.
    use witchy_wir::canonical_safewir::{DictionaryOperation, Operation};
    let checked = checked("fn size_of(let d: Dict(String, Int)) -> Int:\n    d.length()\n\nfn main() -> Int:\n    var d: Dict(String, Int) = dict.with_capacity(4)\n    var i = 0\n    while i < 100:\n        let n = d.get(\"a\") ?? 0\n        let _ = d.insert(\"a\", n + size_of(d))\n        i = i + 1\n    d.get_or(\"a\", 0)\n");
    let lowered = witchy_lower::canonical_pipeline_checks::lowered_borrowed_candidate(&checked);
    let copies = lowered.functions().iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.operations)
        .filter(|operation| matches!(operation, Operation::Dictionary(DictionaryOperation::CloneExact { .. })))
        .count();
    assert_eq!(copies, 0, "a lent dictionary is never copied");
    let source = checked.canonical_source().expect("canonical source");
    witchy_wir::canonical_safewir::check_initial_with_witness(lowered, &source.view())
        .expect("the lent calls are admitted");
}

#[test]
fn a_callee_that_can_keep_its_let_argument_is_not_lent_it() {
    // `keep` can store `d` in its write-back list, so it takes an owned token.
    use witchy_wir::canonical_safewir::{CallArgument, Operation, ParameterOwnership, ValueType};
    let checked = checked(LENT_DICT);
    let lowered = witchy_lower::canonical_pipeline_checks::lowered_borrowed_candidate(&checked);
    let signatures: std::collections::BTreeMap<_, _> = lowered.signatures().iter()
        .map(|signature| (signature.id, signature)).collect();
    let callee_signature: std::collections::BTreeMap<_, _> = lowered.functions().iter()
        .map(|function| (function.id, function.signature)).collect();
    let mut keeping_calls = 0;
    for function in lowered.functions() {
        for operation in function.blocks.iter().flat_map(|block| &block.operations) {
            let Operation::Call { callee, arguments, .. } = operation else { continue };
            let signature = signatures[&callee_signature[callee]];
            let can_keep = signature.contract.parameters.iter().any(|parameter|
                parameter.ownership == ParameterOwnership::WriteBack && matches!(parameter.value_type, ValueType::Ref(_)));
            if !can_keep { continue }
            keeping_calls += 1;
            assert!(!arguments.iter().zip(&signature.contract.parameters).any(|(argument, parameter)|
                matches!(argument, CallArgument::Value(_)) && matches!(parameter.value_type, ValueType::Ref(_))),
                "a callee that can keep a reference is never lent one");
        }
    }
    assert!(keeping_calls > 0, "the program calls a retaining callee");
}

#[test]
fn opaque_scalar_list_writeback_has_a_matching_physical_declaration() {
    use witchy_wir::canonical_safewir::{ParameterOwnership, ResultContract, check_initial_with_witness};
    let _reset = ResetCallbackOptimization;
    for options in [witchy_syntax::opt::OptSet::none(), witchy_syntax::opt::OptSet::all()] {
        witchy_syntax::opt::set_for_tests(Some(options));
        let input = checked("fn invoke(f: fn(var List(Int)) -> (), var values: List(Int)):\n    f(values)\n\nfn main(console: Console):\n    var values = [1]\n    let before = values\n    let update = fn(var values: List(Int)):\n        let snapshot = values\n        values[0] = values[0] + 1\n        console.print(\"${snapshot[0]}\")\n    invoke(update, values)\n    console.print(\"${before[0]} ${values[0]}\")\n");
        let candidate = witchy_lower::canonical_pipeline_checks::lowered_candidate(&input);
        let family = candidate.owner_families().iter().find(|family| {
            candidate.signatures()[family.signature.get() as usize].contract.parameters.iter()
                .any(|parameter| parameter.ownership == ParameterOwnership::UniqueWriteBack)
        }).expect("actual opaque unique writeback callback");
        let body = &candidate.signatures()[family.signature.get() as usize];
        let declaration = &candidate.signatures()[family.declaration_signature.get() as usize];
        assert_eq!(&body.contract.parameters[1..], declaration.contract.parameters);
        assert_eq!(body.contract.results, declaration.contract.results);
        assert!(candidate.functions().iter().any(|function| function.signature == body.id));
        let shared_declaration = candidate.signatures().iter().find(|signature| signature.id != declaration.id
            && signature.parameters == declaration.parameters && signature.results == declaration.results
            && signature.contract.parameters.iter().any(|parameter| parameter.ownership == ParameterOwnership::WriteBack)
            && signature.contract.results.iter().any(|result| matches!(result, ResultContract::Shared { .. })))
            .expect("original type-only Shared ABI").id;
        let source = input.canonical_source().expect("source");
        check_initial_with_witness(candidate.clone(), &source.view()).expect("matching opaque callback ABI");
        let mut families = candidate.owner_families().to_vec();
        families.iter_mut().find(|owner| owner.signature == family.signature).expect("owner family")
            .declaration_signature = shared_declaration;
        let altered = candidate.clone().with_owner_families(families);
        assert!(check_initial_with_witness(altered, &source.view()).is_err(), "mixed body/declaration ownership must refuse");
        compile_checked_module_artifact(&input).expect_lowered("opaque writeback source authority");
    }
}
