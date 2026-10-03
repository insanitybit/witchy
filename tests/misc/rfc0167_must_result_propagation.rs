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

#[test]
fn borrowed_must_parameter_can_return_independent_aggregate() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
must type Ticket:
    Ticket(Int)
fn inspect(let ticket: Ticket) -> Option(Int):
    match ticket:
        Ticket(value) -> Some(value + 1)
fn finish(own ticket: Ticket) -> Int:
    match move ticket:
        Ticket(value) -> value
fn main() -> Int:
    let ticket = Ticket(6)
    let first = match inspect(ticket):
        Some(value) -> value
        None -> 0
    let second = match inspect(ticket):
        Some(value) -> value
        None -> 0
    let local = match ticket:
        Ticket(value) -> value
    let disposed = finish(ticket)
    first * 10 + second + local + disposed
"#).expect("borrowed must resource may produce an independent aggregate");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("borrowed must aggregate result admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["89".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn borrowed_must_match_cannot_extract_an_affine_child_or_return_the_root() {
    let extract = witchy::resolve_std_only_checked(r#"
must type Ticket:
    Ticket(Int)
must type Box:
    Box(Ticket)
fn inspect(let box: Box) -> Int:
    match box:
        Box(ticket) -> 1
fn main() -> Int:
    inspect(Box(Ticket(3)))
"#).expect_err("borrowed match must not extract an affine child");
    assert!(format!("{extract:?}").contains("must-consume"), "{extract:?}");

    let escape = witchy::resolve_std_only_checked(r#"
must type Ticket:
    Ticket(Int)
fn escape(let ticket: Ticket) -> Option(Ticket):
    Some(ticket)
fn main() -> Int:
    0
"#).expect_err("borrowed resource cannot escape in an aggregate result");
    assert!(format!("{escape:?}").contains("must-consume"), "{escape:?}");
}

#[test]
fn borrowed_match_refuses_nested_must_storage_until_child_borrow_has_a_carrier() {
    let error = witchy::resolve_std_only_checked(r#"
must type Ticket:
    Ticket(Int)
must type Box:
    Box(Ticket)
fn inspect(let box: Box) -> Int:
    match box:
        Box(Ticket(value)) -> value
fn finish(own box: Box) -> Int:
    match move box:
        Box(ticket) ->
            match move ticket:
                Ticket(value) -> value
fn main() -> Int:
    let box = Box(Ticket(9))
    let seen = inspect(box)
    seen * 10 + finish(box)
"#).expect_err("nested borrowed must child has no admitted carrier");
    assert!(format!("{error:?}").contains("must-consume"), "{error:?}");
}
