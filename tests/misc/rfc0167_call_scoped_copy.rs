//! A call-scoped input may cross an owned-copy boundary before its result
//! leaves the call. The callback cannot retain the borrowed input itself.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn normal_callback_materializes_call_scoped_string_before_return() {
    let source = r#"
fn copy_through_callback(let text: String) -> String:
    let callback = fn(let value: String) -> String:
        value.owned()
    callback(text)

fn main() -> Int:
    var text = "abc"
    let old = &text
    let independent = copy_through_callback(text)
    text = "x"
    independent.length() * 100 + (*old).length() * 10 + text.length()
"#;
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(source).expect("owned callback result checks");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("owned callback result admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), ["331"]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}
