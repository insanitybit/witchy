use witchy_runtime::runtime::{Capabilities, Runtime, comptime::ComptimeLimits};
use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};
use super::temp_dir::TempDir;
use std::process::Command;

fn owners() -> AuthenticatedModuleOwners {
    let workspace = PackageCoordinate::new(PackageSource::Workspace, "test/ordered-comptime", "1").unwrap();
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", "1").unwrap();
    let assignments = [("generator".into(), ModuleLoadIdentity::new(workspace, ["generator"]).unwrap())]
        .into_iter().chain(witchy_syntax::linker::STD_MODULES.iter().map(|name| (
            (*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).unwrap(),
        )));
    AuthenticatedModuleOwners::from_loader_assignments(assignments).unwrap()
}

fn limits() -> ComptimeLimits {
    ComptimeLimits { fuel: 2_000_000, memory_pages: 64, fresh_identifiers: 4,
        host_output_bytes: 4096, syntax_nodes: 10_000, syntax_bytes: 100_000,
        emitted_nodes: 10_000, emitted_bytes: 100_000 }
}

#[test]
fn admitted_original_blocks_feed_checked_successors_in_normal_and_opt() {
    struct ResetOptimization;
    impl Drop for ResetOptimization { fn drop(&mut self) { witchy::opt::set_for_tests(None); } }
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimization;
    for mode in ["", "mode opt\n"] {
        let source = format!("{mode}comptime:\n    emit_item(quote item:\n        fn answer() -> Int:\n            40\n    )\ncomptime:\n    if answer() == 40:\n        emit_item(quote item:\n            fn result() -> Int:\n                answer() + 2\n        )\nfn main(console: Console):\n    console.print(\"${{result()}}\")\n");
        let checked = witchy::canonical_comptime::check_source_bundle(
            vec![("generator".into(), source.into_bytes())], "generator",
            ["generator".to_owned()].into_iter().collect(), owners(), limits(),
        ).expect("both original blocks execute in order through admitted unoptimized Wasm");
        let artifact = witchy_lower::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("generated destination passes ordinary checking and admission");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, quiet: true, ..Capabilities::none() }, 64).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), ["42"], "independent expectation: generated answer 40 + 2");
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    }
}

#[test]
fn generated_item_is_rechecked_before_successor_executes() {
    let source = "comptime:\n    emit_item(quote item:\n        fn answer() -> Int:\n            \"wrong\"\n    )\ncomptime:\n    fail(\"second block must not execute\")\nfn main() -> Int:\n    0\n";
    let error = match witchy::canonical_comptime::check_source_bundle(
        vec![("generator".into(), source.as_bytes().to_vec())], "generator",
        ["generator".to_owned()].into_iter().collect(), owners(), limits(),
    ) { Ok(_) => panic!("invalid generated item entered destination"), Err(error) => error };
    assert!(error.contains("Int") && error.contains("String"), "{error}");
    assert!(!error.contains("second block must not execute"), "{error}");
}

#[test]
fn generated_block_executes_before_next_original_block() {
    let folder = TempDir::new("admitted-generated-comptime-block");
    let source = folder.join("main.witchy");
    std::fs::write(&source, "comptime:\n    emit_item(quote item:\n        comptime:\n            emit_item(quote item:\n                fn first() -> Int:\n                    40\n            )\n    )\ncomptime:\n    emit_item(quote item:\n        fn answer() -> Int:\n            first() + 2\n    )\nfn main(console: Console):\n    console.print(\"${answer()}\")\n").unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "42\n");
}

#[test]
fn production_source_loader_executes_original_blocks_through_admitted_wasm() {
    let folder = TempDir::new("admitted-comptime-loader");
    let source = folder.join("main.witchy");
    std::fs::write(&source, "fn helper() -> Int:\n    39\nfn relay() -> Int:\n    helper() + 1\nfn unused(console: Console):\n    console.print(\"must stay out of the compiler session\")\ncomptime:\n    if relay() == 40:\n        emit_item(quote item:\n            fn answer() -> Int:\n                40\n        )\ncomptime:\n    emit_item(quote item:\n        fn result() -> Int:\n            answer() + 2\n    )\nfn main(console: Console):\n    console.print(\"${result()}\")\n").unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&source).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "42\n");

    std::fs::write(&source, "comptime:\n    emit_item(quote item:\n        fn answer() -> Int:\n            \"wrong\"\n    )\ncomptime:\n    fail(\"second block must not execute\")\nfn main() -> Int:\n    0\n").unwrap();
    let check = Command::new(env!("CARGO_BIN_EXE_witchy"))
        .arg("check").arg(&source).output().unwrap();
    assert!(!check.status.success(), "invalid generated source was accepted");
    let diagnostic = String::from_utf8_lossy(&check.stderr);
    assert!(diagnostic.contains("Int") && diagnostic.contains("String"), "{diagnostic}");
    assert!(!diagnostic.contains("second block must not execute"), "{diagnostic}");
}

#[test]
fn imported_original_block_feeds_authenticated_entry() {
    let folder = TempDir::new("admitted-imported-comptime");
    let entry = folder.join("main.witchy");
    std::fs::write(folder.join("dep.witchy"), "comptime:\n    emit_item(quote item:\n        pub fn answer() -> Int:\n            42\n    )\n").unwrap();
    std::fs::write(&entry, "import dep\nfn main(console: Console):\n    console.print(\"${dep.answer()}\")\n").unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_witchy")).arg(&entry).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(String::from_utf8_lossy(&run.stdout), "42\n");

    std::fs::write(folder.join("dep.witchy"), "comptime:\n    emit_item(quote item:\n        pub fn answer() -> Int:\n            \"wrong\"\n    )\n").unwrap();
    let expand = Command::new(env!("CARGO_BIN_EXE_witchy"))
        .arg("expand").arg(&entry).output().unwrap();
    assert!(!expand.status.success(), "invalid imported output was displayed as accepted syntax");
    let error = String::from_utf8_lossy(&expand.stderr);
    assert!(error.contains("Int") && error.contains("String"), "{error}");
}

#[test]
fn bare_test_driver_uses_admitted_original_block_helpers() {
    let folder = TempDir::new("admitted-comptime-test-driver");
    let source = folder.join("tests.witchy");
    std::fs::write(&source, "comptime:\n    emit_item(quote item:\n        fn answer() -> Int:\n            42\n    )\nfn test_answer():\n    if answer() != 42:\n        fail(\"wrong admitted helper\")\n").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_witchy"))
        .arg("test").arg(&source).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));

    std::fs::write(&source, "comptime:\n    emit_item(quote item:\n        fn test_forged():\n            ()\n    )\n").unwrap();
    let forged = Command::new(env!("CARGO_BIN_EXE_witchy"))
        .arg("test").arg(&source).output().unwrap();
    assert!(!forged.status.success(), "generated syntax cannot select a raw test target");
    let error = format!("{}{}", String::from_utf8_lossy(&forged.stderr),
        String::from_utf8_lossy(&forged.stdout));
    assert!(error.contains("unknown or ambiguous entry test") || error.contains("declared in the entry source"), "{error}");
}

#[test]
fn imported_original_block_feeds_bare_test_driver() {
    let folder = TempDir::new("admitted-imported-comptime-test");
    let entry = folder.join("tests.witchy");
    let producer = folder.join("dep.witchy");
    std::fs::write(&producer, "comptime:\n    emit_item(quote item:\n        pub fn answer() -> Int:\n            42\n    )\n").unwrap();
    std::fs::write(&entry, "import dep\nfn test_answer():\n    if dep.answer() != 42:\n        fail(\"wrong imported helper\")\n").unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_witchy"))
        .arg("test").arg(&entry).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));

    std::fs::write(&producer, "comptime:\n    emit_item(quote item:\n        pub fn answer() -> Int:\n            \"wrong\"\n    )\n").unwrap();
    let invalid = Command::new(env!("CARGO_BIN_EXE_witchy"))
        .arg("test").arg(&entry).output().unwrap();
    assert!(!invalid.status.success(), "invalid imported output reached the test driver");
    let error = String::from_utf8_lossy(&invalid.stderr);
    assert!(error.contains("Int") && error.contains("String"), "{error}");
}
