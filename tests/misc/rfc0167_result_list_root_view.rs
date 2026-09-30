//! RFC-0167 row N: a normal-mode Result view retains its nested List root.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn normal_result_list_view_keeps_the_old_child_after_owner_replacement() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
fn relay(v: &Result(List(Int), String)) -> &Result(List(Int), String):
    v

fn read(v: &Result(List(Int), String)) -> Int:
    match *v:
        Ok(xs) -> xs[0] + xs.length()
        Err(s) -> string.length(s)

fn main() -> Int:
    var r: Result(List(Int), String) = Ok([2, 3])
    let old = relay(&r)
    r = Err("bad")
    read(old) * 10 + read(&r)
"#).expect("normal nested Result view checks");
    // Old Ok list contributes 2 + 2 = 4; current Err string has length 3.
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("normal nested Result view admits");
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

#[test]
fn region_born_result_list_root_cannot_escape_in_a_view() {
    let error = witchy::resolve_std_only_checked(r#"
fn main() -> Int:
    let old = region -> &Result(List(Int), String):
        let local: Result(List(Int), String) = Ok([2, 3])
        &local
    0
"#).err().map(|error| error.to_string())
        .expect("region-born Result root must be refused");
    assert!(error.contains("region-born"), "region refusal identifies the root: {error}");
}
