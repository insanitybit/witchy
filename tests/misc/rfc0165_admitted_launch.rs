//! RFC-0165 launch boundary: an admitted canonical artifact runs through the
//! ordinary runtime API, not only through a test that calls `f0` by hand.
//!
//! Canonical modules export `f<id>` and no parameterless `run`; the runtime
//! enters the validated entry recorded at admission, mints one host token per
//! declared capability parameter, and prints an `Int` result under the frozen
//! exit-code convention. These are the two program shapes the compact
//! foundation subset promises to run end to end.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

fn run_admitted(source: &str) -> Vec<String> {
    let checked = witchy::resolve_std_only_checked(source).expect("fixture checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("the foundation subset admits this fixture");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities {
                print: true,
                print_int: true,
                quiet: true,
                ..Default::default()
            },
            256,
        )
        .expect("spawn admitted artifact");
    vm.run().expect("run admitted artifact through the runtime entry");
    vm.output()
}

#[test]
fn admitted_console_program_runs_through_the_runtime_entry() {
    let output = run_admitted("fn main(console: Console):\n    console.print(\"ok\")\n");
    assert_eq!(output, vec!["ok".to_string()]);
}

#[test]
fn admitted_int_result_is_printed_under_the_exit_code_convention() {
    let output = run_admitted("fn main() -> Int:\n    40 + 2\n");
    assert_eq!(output, vec!["42".to_string()]);
}

#[test]
fn admitted_entry_without_a_console_grant_is_refused_before_it_runs() {
    let checked = witchy::resolve_std_only_checked(
        "fn main(console: Console):\n    console.print(\"ok\")\n",
    )
    .expect("fixture checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("the foundation subset admits this fixture");
    let mut runtime = Runtime::batch().expect("runtime");
    // Without the `print` grant the `witchy.print` import is absent, so the
    // module cannot even instantiate: the structural check fires first, and
    // no Console token is ever minted.
    assert!(runtime
        .spawn_admitted(&artifact, Capabilities { quiet: true, ..Default::default() }, 256)
        .is_err());
}
