//! A dictionary storing views retains its old owner graph through a root view.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn dictionary_of_views_keeps_the_root_snapshot_after_replacement() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
fn count(v: &Dict(String, &String)) -> Int:
    (*v).length() * 100 + (*((*v).at("a"))).length() * 10

fn main() -> Int:
    let old = "ab"
    let newer = "xyz"
    var d: Dict(String, &String) = dict.new()
    let _ = d.insert("a", &old)
    let snapshot = &d
    d = dict.new()
    let _ = d.insert("b", &newer)
    count(snapshot) * 10 + d.length()
"#).expect("dictionary of views has an authenticated root view");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("dictionary graph root admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["1201".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}
