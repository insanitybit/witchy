//! RFC-0179 R4: `@large_heap` on `main` declares the program's heap mode. The
//! admitted module declares the mode's memory maximum, a large program runs on
//! the large-reservation engine, and a refused growth names the limit hit.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

fn admit(source: &str) -> witchy_wir::AdmittedCanonicalArtifact {
    let checked = witchy::resolve_std_only_checked(source).expect("check program");
    codegen::compile_checked_module_artifact(&checked).expect_lowered("compile program")
}

fn declared_maximum_pages(wasm: &[u8]) -> Option<u64> {
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        if let wasmparser::Payload::MemorySection(memories) = payload.expect("valid module") {
            return memories.into_iter().next().expect("one memory").expect("memory type").maximum;
        }
    }
    None
}

fn console() -> Capabilities {
    Capabilities { print: true, print_int: true, quiet: true, ..Default::default() }
}

#[test]
fn large_heap_program_declares_32_gib_and_runs_on_admitted_wasm() {
    let artifact = admit("@large_heap\nfn main(console: Console):\n    let xs = [1, 2, 3]\n    console.print(\"${xs.length()}\")\n");
    assert_eq!(artifact.heap_mode(), witchy_wir::HeapMode::Large);
    assert_eq!(declared_maximum_pages(artifact.wasm()), Some(524_288));
    let mut runtime = Runtime::batch().expect("create runtime");
    let mut vm = runtime.spawn_admitted(&artifact, console(), 64).expect("spawn on the large engine");
    vm.run().expect("run program");
    assert_eq!(vm.output(), ["3"]);
}

#[test]
fn standard_program_keeps_its_4_gib_maximum() {
    let artifact = admit("fn main(console: Console):\n    console.print(\"small\")\n");
    assert_eq!(artifact.heap_mode(), witchy_wir::HeapMode::Standard);
    assert_eq!(declared_maximum_pages(artifact.wasm()), Some(65_536));
}

#[test]
fn refused_growth_names_the_run_limit_and_the_marker() {
    let artifact = admit("fn main(console: Console):\n    var xs: List(Int) = []\n    for i in 0..400000:\n        xs.push(i)\n    console.print(\"${xs.length()}\")\n");
    let mut runtime = Runtime::batch().expect("create runtime");
    // 32 pages: a 2 MiB run limit the 3.2 MB list cannot fit.
    let mut vm = runtime.spawn_admitted(&artifact, console(), 32).expect("spawn program");
    let error = vm.run().expect_err("the list outgrows the run limit").root_cause().to_string();
    assert!(error.contains("out of memory: the 2 MiB memory limit for a run was reached"), "{error}");
    assert!(error.contains("mark `main` with `@large_heap` to ask for a larger heap"), "{error}");
}
