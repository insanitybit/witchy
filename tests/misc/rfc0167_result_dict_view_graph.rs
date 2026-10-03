//! A Result root retains a dictionary whose value is itself a view.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn result_root_with_dictionary_of_views_keeps_its_snapshot() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
fn read(v: &Result(Dict(String, &String), String)) -> Int:
    match *v:
        Ok(d) -> (*d.at("a")).length() * 10 + d.length()
        Err(s) -> s.length()

fn main() -> Int:
    let text = "ab"
    var d: Dict(String, &String) = dict.new()
    let _ = d.insert("a", &text)
    var r: Result(Dict(String, &String), String) = Ok(d)
    let old = &r
    r = Err("bad")
    read(old) * 10 + read(&r)
"#).expect("Result dictionary view graph checks");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("Result dictionary graph admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["213".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}
