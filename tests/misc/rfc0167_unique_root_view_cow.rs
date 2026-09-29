//! A normal-mode unique root may lawfully copy when a retained view requires
//! an immutable snapshot before whole-root mutation.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn unique_list_mutation_keeps_a_retained_view_snapshot() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let source = r#"fn main(console: Console):
    var values: unique List(Int) = [1, 2]
    let previous = &values
    values.push(3)
    console.print("${(*previous).length()} ${values.length()} ${(*previous)[0]}")
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("normal unique view checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("normal unique view admits through canonical SafeWIR");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print: true, print_int: true, quiet: true, ..Capabilities::none() }, 128,
    ).expect("admitted Wasm spawn");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), ["2 3 1"]);
    assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0));
}
