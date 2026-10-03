//! A borrowed match may inspect scalar data beneath a selected affine variant.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn borrowed_nested_sum_reads_each_variant_without_transferring_the_child() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
must type Ticket:
    Good(Int)
    Bad(Int)
must type Box:
    Box(Ticket)

fn inspect(let box: Box) -> Int:
    match box:
        Box(Good(value)) -> value + 1
        Box(Bad(value)) -> value + 2

fn finish(own box: Box) -> Int:
    match move box:
        Box(ticket) ->
            match move ticket:
                Good(value) -> value
                Bad(value) -> value

fn main() -> Int:
    let good = Box(Good(3))
    let bad = Box(Bad(7))
    let first = inspect(good)
    let second = inspect(bad)
    first * 1000 + second * 100 + finish(good) * 10 + finish(bad)
"#).expect("borrowed nested sum checks");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("borrowed nested sum admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["4937".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}
