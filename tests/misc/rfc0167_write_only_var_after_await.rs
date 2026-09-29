//! A `var` local assigned after suspension remains bound even if no later read
//! keeps it live. The replacement string must still be released at task exit.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

#[test]
fn write_only_var_after_await_is_bound_and_released() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;

    let source = r#"from task import yield_now

async fn main(console: Console):
    var value = "before"
    console.print(value)
    yield_now().await
    value = "after" + "!"
    console.print("done")
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("source checks");
    let (artifact, ()) = codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
        .expect_lowered("source admits through canonical SafeWIR");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print: true, print_int: true, quiet: true, ..Capabilities::none() },
            128,
        )
        .expect("admitted Wasm spawn");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), ["before", "done"]);
    assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0));
}
