//! RFC-0129 row 6: cooperative tasks and parallel workers keep distinct costs.

use std::collections::BTreeSet;

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};

const COOPERATIVE_MAP: &str = r#"
import chan

async fn square(n: Int) -> Int:
    chan.yield_now().await
    n * n

async fn main(console: Console):
    let values = chan.par_map([5, 3, 8, 1], square).await
    console.print("${values}")
"#;

const PARALLEL_WORKER_MAP: &str = r#"
import vm

fn square(n: Int) -> Int:
    n * n

fn main(console: Console):
    let values = vm.par_map([5, 3, 8, 1], square)
    console.print("${values}")
"#;

const SEQUENTIAL_WORKER_FALLBACK: &str = r#"
import vm

fn main(console: Console):
    let bias = 10
    let values = vm.par_map([5, 3, 8, 1], fn(n: Int): n + bias)
    console.print("${values}")
"#;

fn run_on_both_backends(source: &str) -> (Vec<String>, Vec<u8>) {
    let checked = witchy::resolve_std_only_checked(source)
        .expect("RFC-0129 row-6 source must check");
    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("run RFC-0129 row-6 source on the interpreter");
    let wasm = codegen::compile_checked_module_binary(&checked)
        .expect_lowered("compile RFC-0129 row-6 source");
    let mut runtime = Runtime::batch().expect("create RFC-0129 row-6 runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities {
                print: true,
                quiet: true,
                ..Default::default()
            },
            256,
        )
        .expect("spawn RFC-0129 row-6 source");
    actor.run().expect("run RFC-0129 row-6 compiled Wasm");
    assert_eq!(actor.output(), interpreted, "row-6 backends must agree");
    (interpreted, wasm)
}

fn witchy_imports(wasm: &[u8]) -> BTreeSet<String> {
    let mut imports = BTreeSet::new();
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        if let wasmparser::Payload::ImportSection(section) = payload.expect("valid Wasm") {
            for import in section.into_imports() {
                let import = import.expect("valid Wasm import");
                if import.module == "witchy" {
                    imports.insert(import.name.to_string());
                }
            }
        }
    }
    imports
}

fn worker_imports(imports: &BTreeSet<String>) -> BTreeSet<String> {
    imports
        .iter()
        .filter(|name| name.starts_with("vm_par_map"))
        .cloned()
        .collect()
}

#[test]
fn rfc0129_acceptance_row_6_cooperative_and_parallel_maps_use_distinct_boundaries() {
    let expected = vec!["[25, 9, 64, 1]".to_string()];

    let (cooperative_output, cooperative_wasm) = run_on_both_backends(COOPERATIVE_MAP);
    assert_eq!(cooperative_output, expected, "cooperative map keeps input order");
    let cooperative_workers = worker_imports(&witchy_imports(&cooperative_wasm));
    assert!(
        cooperative_workers.is_empty(),
        "chan.par_map must stay inside its cooperative executor: {cooperative_workers:?}",
    );

    let (parallel_output, parallel_wasm) = run_on_both_backends(PARALLEL_WORKER_MAP);
    assert_eq!(parallel_output, expected, "parallel worker map keeps input order");
    let parallel_workers = worker_imports(&witchy_imports(&parallel_wasm));
    assert_eq!(
        parallel_workers,
        BTreeSet::from([
            "vm_par_map_run".to_string(),
            "vm_par_map_write".to_string(),
        ]),
        "a direct scalar vm.par_map must pay the explicit worker-VM host boundary",
    );

    let (fallback_output, fallback_wasm) = run_on_both_backends(SEQUENTIAL_WORKER_FALLBACK);
    assert_eq!(fallback_output, ["[15, 13, 18, 11]"], "fallback remains ordered");
    let fallback_workers = worker_imports(&witchy_imports(&fallback_wasm));
    assert!(
        fallback_workers.is_empty(),
        "a capturing callback cannot silently cross the worker boundary: {fallback_workers:?}",
    );
}
