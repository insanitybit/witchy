//! Checked storage context follows continuing value edges, not just constructors.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

const WORKER: &str = r#"type Box(a):
    value: a
    count: Int
pub fn take(own box: Box(unique String)) -> Int:
    match move box:
        Box(value, count) -> value.length() * 10 + count
"#;
const MAIN: &str = r#"import worker
fn choose(flag: Bool, selector: Int) -> Int:
    let a = worker.take(if flag: worker.Box("abc", 4) else: worker.Box("q", 2))
    let b = worker.take(match selector:
        0 -> worker.Box("xy", 5)
        _ -> worker.Box("z", 6)
    )
    let c: worker.Box(unique String) = match selector:
        _ ->
            let count = 3
            worker.Box("uvw", count)
    let d = worker.take(move c)
    a * 10000 + b * 100 + d
fn surviving(flag: Bool) -> Int:
    let box: worker.Box(unique String) = if flag:
        worker.Box("a", 1)
    else:
        return 5
    worker.take(move box)
fn main() -> Int:
    choose(true, 0) * 1000000 + choose(false, 1) + surviving(true) + surviving(false)
"#;

fn checked(main_mode: &str, worker_mode: &str, main: &str) -> Result<witchy_types::pipeline::CheckedModule, String> {
    witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("worker".into(), format!("{worker_mode}{WORKER}").into_bytes()),
             ("main".into(), format!("{main_mode}{main}").into_bytes())],
        "main", witchy_interp::comptime::EXPANDER,
    ).map_err(|error| error.to_string())
}

const MODES: [(&str, &str); 3] = [("", ""), ("mode opt\n", "mode opt\n"), ("", "mode opt\n")];

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn control_flow_storage_reaches_if_match_and_block_tails() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    for (main_mode, worker_mode) in MODES {
        let source = checked(main_mode, worker_mode, MAIN).expect("checked contextual storage");
        let artifact = witchy::codegen::compile_checked_module_artifact(&source)
            .expect_lowered("unoptimized control-flow storage admission");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128,
        ).expect("admitted unoptimized Wasm");
        vm.run().expect("control-flow storage execution");
        // true/0: 34*10000+25*100+33=342533; false/1:121633.
        // Continuing branch adds11, early function return adds5 (not a Box).
        assert_eq!(vm.output(), vec!["342533121649"]);
        assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0));
    }
}

#[test]
fn control_flow_storage_cannot_duplicate_a_consumed_branch_input() {
    for (main_mode, worker_mode) in MODES {
        let error = checked(main_mode, worker_mode, r#"import worker
fn duplicate(flag: Bool, own box: worker.Box(unique String)) -> Int:
    let first = worker.take(if flag: move box else: move box)
    first + worker.take(move box)
fn main() -> Int:
    duplicate(true, worker.Box("x", 1))
"#).expect_err("context does not manufacture a second affine owner");
        assert!(error.contains("consumed") || error.contains("moved") || error.contains("transferred"), "{error}");
    }
}
