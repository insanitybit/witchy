//! RFC-0167 row N: a normal closure retains a view into a Result-owned List.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn normal_closure_keeps_the_result_list_snapshot_after_replacement() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
fn read(v: &Result(List(Int), String)) -> Int:
    match *v:
        Ok(xs) -> xs[0] + xs.length()
        Err(s) -> string.length(s)

fn main() -> Int:
    var r: Result(List(Int), String) = Ok([2, 3])
    let old = &r
    let action = fn() -> Int: read(old)
    r = Err("bad")
    action() * 10 + read(&r)
"#).expect("normal closure captures a nested Result view");
    // The closure reads 2 + 2 from the old List; current Err has length 3.
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("captured Result view admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["43".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}
