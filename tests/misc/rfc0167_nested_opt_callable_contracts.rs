//! Source-linked stored callback contracts reach admitted unoptimized Wasm.
use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn recursive_stored_callback_contracts_execute_without_optional_optimization() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let worker = r#"mode opt

type Holder(a):
    value: a
    callback: fn(Int) -> Int

pub fn plus_one(value: Int) -> Int:
    value + 1

pub fn invoke(holder: Holder(a)) -> Int:
    (holder.callback)(41)

pub fn nested(pair: (Holder(Int), Int)) -> Int:
    (pair.0.callback)(41) + pair.1

pub fn relay(holder: Holder(Int)) -> Holder(Int):
    holder

type Route:
    Routed(Holder(Int))
    Empty

pub fn routed(route: Route) -> Int:
    match route:
        Routed(holder) -> (holder.callback)(41)
        Empty -> 0

pub fn sequence(callbacks: List(fn(Int) -> Int)) -> Int:
    callbacks[0](41)
"#;
    let main = r#"import worker

fn main() -> Int:
    let holder = worker.Holder(7, worker.plus_one)
    worker.invoke(holder) + worker.nested((holder, 3)) + worker.invoke(worker.relay(holder)) + worker.routed(worker.Routed(holder)) + worker.sequence([worker.plus_one])
"#;
    let checked = witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("worker".into(), worker.as_bytes().to_vec()), ("main".into(), main.as_bytes().to_vec())],
        "main", witchy_interp::comptime::EXPANDER,
    ).expect("source-linked recursive callback contracts");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("canonical SafeWIR admission without optional optimization");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact, Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128,
    ).expect("admitted unoptimized Wasm instance");
    vm.run().expect("admitted Wasm execution");
    // Generic holder 42 + tuple (42 + 3) + returned holder 42 + sum 42 + list 42.
    assert_eq!(vm.output(), vec!["213"]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}
