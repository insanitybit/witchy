//! RFC-0167 row N: a normal tuple view retains nested String and Result data.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn normal_tuple_view_retains_a_string_list_and_result_after_replacement() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
fn read(v: &(List(String), Result(Int, String))) -> Int:
    let xs = (*v).0
    let r = (*v).1
    let n = match r:
        Ok(value) -> value
        Err(s) -> string.length(s)
    string.length(xs[0]) * 10 + n

fn main() -> Int:
    var p: (List(String), Result(Int, String)) = (["ab", "c"], Ok(7))
    let old = &p
    p = (["z"], Err("bad"))
    read(old) * 100 + read(&p)
"#).expect("normal tuple view checks");
    // Old tuple contributes 2*10+7=27; current tuple contributes 1*10+3=13.
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("nested tuple view admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["2713".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn tuple_view_with_nested_list_of_views_has_an_authenticated_graph_carrier() {
    witchy::resolve_std_only_checked(r#"
fn main() -> Int:
    let n = 1
    let p: (List(&Int), Result(Int, String)) = ([&n], Ok(1))
    let v = &p
    0
"#).expect("nested view payload has a checked graph carrier");
}
