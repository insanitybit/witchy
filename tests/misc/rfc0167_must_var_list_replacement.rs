//! A var write-back transfers the successor must list to the caller.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn var_list_replacement_preserves_must_disposition_on_both_sides() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
import list
must type Ticket:
    Ticket(Int)

fn finish(own ticket: Ticket) -> Int:
    match move ticket:
        Ticket(value) -> value

fn drain(own tickets: List(Ticket)) -> Int:
    match list.__pop_extract(tickets):
        Some(ticket) -> finish(ticket) + drain(tickets)
        None -> 0

fn replace(var tickets: List(Ticket)) -> Int:
    match list.__pop_extract(tickets):
        Some(ticket) -> finish(ticket) + replace(tickets)
        None ->
            tickets = [Ticket(3)]
            0

fn main() -> Int:
    var tickets = [Ticket(1), Ticket(2)]
    let old = replace(tickets)
    old * 10 + drain(tickets)
"#).expect("must list replacement checks");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("must list replacement admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["33".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn exact_empty_proof_cannot_authorize_later_or_repeated_replacement() {
    let prefix = r#"import list
must type Ticket:
    Ticket(Int)
fn finish(own ticket: Ticket) -> Int:
    match move ticket:
        Ticket(value) -> value
fn replace(var tickets: List(Ticket)) -> Int:
    match list.__pop_extract(tickets):
        Some(ticket) -> finish(ticket) + replace(tickets)
        None ->
"#;
    for body in [
        "            tickets = [Ticket(3)]\n            tickets = [Ticket(4)]\n            0\n",
        "            let pause = 0\n            tickets = [Ticket(3)]\n            pause\n",
    ] {
        let source = format!("{prefix}{body}fn main() -> Int:\n    var tickets = [Ticket(1)]\n    replace(tickets)\n");
        let error = witchy::resolve_std_only_checked(&source)
            .expect_err("a borrowed must list needs a fresh exact empty proof");
        assert!(error.to_string().contains("assignment would replace borrowed must-consume"), "{error}");
    }
}
