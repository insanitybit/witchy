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

#[test]
fn admitted_loop_holding_a_capability_runs_every_iteration() {
    // The checker once refused any loop in a function holding a capability:
    // the header parameter carrying `console` had no linear-memory object
    // state, and the edge check demanded one for every non-scalar parameter.
    let output = run_admitted(
        "fn main(console: Console):\n    var i = 0\n    while i < 3:\n        console.print(\"tick\")\n        i = i + 1\n",
    );
    assert_eq!(output, vec!["tick".to_string(); 3]);
}

#[test]
fn admitted_explicit_let_parameters_run() {
    let output = run_admitted(
        "fn twice(let n: Int) -> Int:\n    n * 2\n\nfn main() -> Int:\n    twice(21)\n",
    );
    assert_eq!(output, vec!["42".to_string()]);
}

#[test]
fn admitted_if_without_else_and_effect_arms_run() {
    let output = run_admitted(
        "fn main(console: Console):\n    let a = 1\n    if a > 0:\n        console.print(\"yes\")\n",
    );
    assert_eq!(output, vec!["yes".to_string()]);
    let output = run_admitted(
        "fn main(console: Console):\n    let a = 7\n    if a > 5:\n        console.print(\"big\")\n    else:\n        console.print(\"small\")\n",
    );
    assert_eq!(output, vec!["big".to_string()]);
}

#[test]
fn admitted_short_circuit_operators_run() {
    let output = run_admitted(
        "fn main(console: Console):\n    let a = 1\n    if a > 0 && a < 5:\n        console.print(\"in\")\n    else:\n        console.print(\"out\")\n    if a > 3 || a == 1:\n        console.print(\"or\")\n",
    );
    assert_eq!(output, vec!["in".to_string(), "or".to_string()]);
}

#[test]
fn admitted_string_literals_cross_owned_parameters_and_returns() {
    // A literal has no reference-count token; passing or returning one under
    // the owned ABI materializes a fresh copy rather than cloning static data.
    let output = run_admitted(
        "pub fn shout(name: String) -> String:\n    \"hi \" + name\n\nfn greet() -> String:\n    \"hello\"\n\nfn keep(own value: String) -> String:\n    value\n\nfn main(console: Console):\n    console.print(shout(\"x\"))\n    console.print(greet())\n    console.print(keep(\"ok\"))\n",
    );
    assert_eq!(output, vec!["hi x".to_string(), "hello".to_string(), "ok".to_string()]);
}
