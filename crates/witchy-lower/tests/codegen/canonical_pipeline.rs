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
fn run_callback(source_text: &str) -> Result<i64, String> {
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
    instance.get_typed_func::<(), i64>(&mut store, &format!("f{}", main.get()))
        .expect("callback main export").call(&mut store, ()).map_err(|error| error.root_cause().to_string())
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
