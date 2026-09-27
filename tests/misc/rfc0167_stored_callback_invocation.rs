//! RFC-0167: invocation uses actual callback provenance, not its holder's module.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn normal_and_authenticated_opt_stored_callbacks_execute_without_optional_optimization() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let worker = r#"mode opt

type Holder:
    callback: fn(Int) -> Int

pub fn plus_one(value: Int) -> Int:
    value + 1

pub fn apply(callback: fn(Int) -> Int, value: Int) -> Int:
    callback(value)

pub fn local() -> Int:
    let holder = Holder(plus_one)
    let callback = holder.callback
    callback(41) + (holder.callback)(41)
"#;
    let main = r#"import worker

fn ordinary(value: Int) -> Int:
    value + 1

fn main() -> Int:
    let holder = worker.Holder(ordinary)
    let callback = holder.callback
    callback(41) + worker.local() + worker.apply(worker.plus_one, 41)
"#;
    let checked = witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("worker".into(), worker.as_bytes().to_vec()), ("main".into(), main.as_bytes().to_vec())],
        "main",
        witchy_interp::comptime::EXPANDER,
    ).expect("normal holder use and actual opt callback provenance both remain legal");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("canonical SafeWIR admission without optional optimization");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("admitted unoptimized Wasm instance");
    vm.run().expect("admitted Wasm execution");
    // Every call adds one to 41: normal holder 42 + opt local 84 + opt formal 42.
    assert_eq!(vm.output(), vec!["168"], "independently computed callback result");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}
