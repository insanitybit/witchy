//! A closure may retain a copyable capability alongside ordinary owned data.
use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn captured_capability_and_list_keep_their_checked_host_authority() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let source = r#"
fn main(console: Console):
    let state = (console, [4, 5])
    let report = fn(): state.0.print("${state.1.length()}")
    report()
"#;
    let checked = witchy::resolve_std_only_checked(source)
        .expect("normal closure aggregate must type-check");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("normal capability closure must admit");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities { print: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), ["2"]);
    assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0));
}
