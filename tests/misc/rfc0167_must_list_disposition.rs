//! RFC-0167: a list of must values is discharged by exact consuming pop.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn exact_pop_drains_a_must_list_on_admitted_wasm() {
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

fn main() -> Int:
    drain([Ticket(1), Ticket(2)])
"#).expect("exact pop proves the remaining must list empty");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("must list drain admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["3".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}
