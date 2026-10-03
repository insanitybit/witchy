//! A must-bearing error survives `?` until the caller disposes it.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn result_error_propagates_a_must_list_to_its_caller() {
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

fn produce(ok: Bool) -> Result(Int, List(Ticket)):
    if ok: Ok(2) else: Err([Ticket(7), Ticket(8)])

fn relay(ok: Bool) -> Result(Int, List(Ticket)):
    let n = produce(ok)?
    Ok(n + 1)

fn read(ok: Bool) -> Int:
    match relay(ok):
        Ok(n) -> n
        Err(tickets) -> drain(tickets)

fn main() -> Int:
    read(false) * 10 + read(true)
"#).expect("must-bearing error propagates through question-mark");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("must-bearing Result error admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["153".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}
