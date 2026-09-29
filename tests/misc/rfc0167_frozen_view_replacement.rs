//! Normal-mode frozen values allow whole replacement while a rooted view keeps
//! the earlier immutable snapshot. Field mutation remains refused.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn frozen_record_replacement_preserves_an_older_view() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let source = r#"type Box:
    value: Int

fn main(console: Console):
    var b: frozen Box = Box(1)
    let previous: &frozen Box = &b
    b = Box(2)
    console.print("${(*previous).value} ${b.value}")
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("normal frozen view checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("normal frozen view admits through canonical SafeWIR");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print: true, print_int: true, quiet: true, ..Capabilities::none() }, 128,
    ).expect("admitted Wasm spawn");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), ["1 2"]);
    assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0));
}

#[test]
fn frozen_record_field_mutation_is_refused_in_normal_mode() {
    let source = "type Box:\n    value: Int\n\nfn main() -> Int:\n    var b: frozen Box = Box(1)\n    b.value = 2\n    b.value\n";
    let error = witchy::resolve_std_only_checked(source).expect_err("frozen field mutation must refuse");
    assert!(error.to_string().contains("cannot mutate through frozen value"), "{error}");
}
