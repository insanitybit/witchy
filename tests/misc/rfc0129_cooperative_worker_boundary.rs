//! RFC-0129 row 6: cooperative tasks and parallel workers keep distinct APIs,
//! capability boundaries, and boundary costs.

use std::collections::BTreeSet;

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen};

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

fn run_admitted(source: &str) -> (Vec<String>, Vec<u8>, usize) {
    let checked = witchy::resolve_std_only_checked(source)
        .expect("RFC-0129 row-6 source must check");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admit RFC-0129 row-6 source");
    let wasm = artifact.wasm().to_vec();
    let mut runtime = Runtime::batch().expect("create RFC-0129 row-6 runtime");
    let mut actor = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print: true, quiet: true, ..Default::default() },
            256,
        )
        .expect("spawn admitted RFC-0129 row-6 source");
    actor.run().expect("run RFC-0129 row-6 admitted Wasm");
    assert_eq!(actor.canonical_heap_leaks().expect("heap audit"), Some(0));
    (actor.output(), wasm, actor.admitted_zero_grant_worker_launches())
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

    let (cooperative_output, cooperative_wasm, cooperative_launches) = run_admitted(COOPERATIVE_MAP);
    assert_eq!(cooperative_launches, 0, "cooperative work launches no worker VMs");
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

    let (parallel_output, parallel_wasm, parallel_launches) = run_admitted(PARALLEL_WORKER_MAP);
    assert!(parallel_launches > 0, "scalar par_map must launch a zero-grant worker VM");
    assert_eq!(parallel_output, ["[25, 9, 64, 1]"], "parallel worker map keeps input order");
    let parallel_workers = worker_imports(&witchy_imports(&parallel_wasm));
    assert_eq!(
        parallel_workers,
        BTreeSet::from([
            "vm_par_map_admitted_run".to_string(),
            "vm_par_map_admitted_write".to_string(),
        ]),
        "a direct scalar vm.par_map has a measured two-import worker-VM boundary",
    );

    let (fallback_output, fallback_wasm, fallback_launches) = run_admitted(SEQUENTIAL_WORKER_FALLBACK);
    assert_eq!(fallback_launches, 0, "capturing callback launches no worker VMs");
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
    // Legacy raw-codegen ABI evidence only; canonical vm.with_dir admission is
    // tracked separately from the admitted par_map worker boundary above.
    let explicit_wasm = codegen::compile_module_binary(explicit_checked.module())
        .expect_lowered("compile the legacy explicit Dir worker source");
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
