//! RFC-0129 row 6: cooperative tasks and parallel workers keep distinct APIs,
//! capability boundaries, and boundary costs.

use std::collections::BTreeSet;

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};

const COOPERATIVE_MAP: &str = r#"
import chan

async fn square(n: Int) -> Int:
    chan.yield_now().await
    n * n

async fn visible_square(console: Console, n: Int) -> Int:
    console.print("task ${n}")
    square(n).await

async fn main(console: Console):
    let values = chan.par_map([5, 3, 8, 1], fn(n): visible_square(console, n)).await
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

fn visible_square(console: Console, n: Int) -> Int:
    console.print("parent ${n}")
    n * n

fn main(console: Console):
    let values = vm.par_map([5, 3, 8, 1], fn(n): visible_square(console, n))
    console.print("${values}")
"#;

const EXPLICIT_DIR_WORKER: &str = r#"
import bytes
import vm

fn echo(_dir: Dir, input: Bytes) -> Bytes:
    input

fn main(dir: Dir):
    let output = vm.with_dir(dir, echo, bytes.from_string("boundary"))
    let _ = output
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

fn run_grant_names(source: &str) -> BTreeSet<String> {
    let checked = witchy::resolve_std_only_checked(source)
        .expect("RFC-0129 row-6 capability source must check");
    witchy::capabilities::run_grant(checked.module())
        .keys()
        .map(|name| (*name).to_string())
        .collect()
}

#[test]
fn rfc0129_acceptance_row_6_cooperative_and_parallel_maps_use_distinct_boundaries() {
    let cooperative_expected = [
        "task 5",
        "task 3",
        "task 8",
        "task 1",
        "[25, 9, 64, 1]",
    ];

    let (cooperative_output, cooperative_wasm) = run_on_both_backends(COOPERATIVE_MAP);
    assert_eq!(
        cooperative_output,
        cooperative_expected,
        "cooperative tasks may use an explicitly passed Console and keep input order",
    );
    assert_eq!(
        run_grant_names(COOPERATIVE_MAP),
        BTreeSet::from(["Console".to_string()]),
        "cooperative work receives only main's explicit Console grant",
    );
    let cooperative_workers = worker_imports(&witchy_imports(&cooperative_wasm));
    assert!(
        cooperative_workers.is_empty(),
        "chan.par_map must stay inside its cooperative executor: {cooperative_workers:?}",
    );

    let (parallel_output, parallel_wasm) = run_on_both_backends(PARALLEL_WORKER_MAP);
    assert_eq!(parallel_output, ["[25, 9, 64, 1]"], "parallel worker map keeps input order");
    let parallel_workers = worker_imports(&witchy_imports(&parallel_wasm));
    assert_eq!(
        parallel_workers,
        BTreeSet::from([
            "vm_par_map_run".to_string(),
            "vm_par_map_write".to_string(),
        ]),
        "a direct scalar vm.par_map has a measured two-import worker-VM boundary",
    );

    let (fallback_output, fallback_wasm) = run_on_both_backends(SEQUENTIAL_WORKER_FALLBACK);
    assert_eq!(
        fallback_output,
        ["parent 5", "parent 3", "parent 8", "parent 1", "[25, 9, 64, 1]"],
        "a capability-bearing callback remains ordered in its parent VM",
    );
    assert_eq!(
        run_grant_names(SEQUENTIAL_WORKER_FALLBACK),
        BTreeSet::from(["Console".to_string()]),
        "capturing Console does not manufacture a worker grant",
    );
    let fallback_workers = worker_imports(&witchy_imports(&fallback_wasm));
    assert!(
        fallback_workers.is_empty(),
        "a capability-capturing callback cannot silently cross the worker boundary: {fallback_workers:?}",
    );

    let explicit_checked = witchy::resolve_std_only_checked(EXPLICIT_DIR_WORKER)
        .expect("the explicit Dir worker source must check");
    let explicit_wasm = codegen::compile_checked_module_binary(&explicit_checked)
        .expect_lowered("compile the explicit Dir worker source");
    let explicit_imports = witchy_imports(&explicit_wasm);
    assert!(
        explicit_imports.contains("vm_with_dir_run"),
        "vm.with_dir must retain its explicit isolated-worker adapter: {explicit_imports:?}",
    );
    assert_eq!(
        run_grant_names(EXPLICIT_DIR_WORKER),
        BTreeSet::from(["Dir".to_string()]),
        "the worker receives exactly the Dir named by the API",
    );
}
