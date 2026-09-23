//! RFC-0167 row K: views travel through generator frames and channels.
//!
//! A view is a value-semantic snapshot that retains its own root (section 5),
//! so a generator frame or a channel buffer holding one cannot outlive
//! anything it depends on. A `gen fn` takes view parameters and yields views;
//! a `Receiver(&T)` / `Sender(&T)` carries views between tasks, and the
//! receiving task reads the sender's snapshot (tasks share no memory; the
//! retained root is immutable). Retired lifetime-bearing references stay
//! refused, and a region-born view still cannot leave its region inside a
//! generator's iterator or a task recipe (section 8; BUG-646).
//!
//! Evidence: independent expected output and admitted unoptimized Wasm with
//! a zero-leak heap audit. The async cases share ONE program (one compile).

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

fn assert_output(label: &str, source: &str, expected: &[&str]) {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;

    let checked = witchy::resolve_std_only_checked(source)
        .unwrap_or_else(|error| panic!("{label} type-checks: {error}"));
    let expected: Vec<String> = expected.iter().map(|line| (*line).to_owned()).collect();
    let (artifact, ()) = codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
        .expect_lowered(&format!("{label} admits through canonical SafeWIR"));

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print: true, print_int: true, quiet: true, ..Capabilities::none() },
            128,
        )
        .unwrap_or_else(|error| panic!("{label} admitted Wasm spawn: {error}"));
    vm.run().unwrap_or_else(|error| panic!("{label} admitted Wasm execution: {error}"));
    assert_eq!(vm.output(), expected, "{label} admitted Wasm matches the expectation");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0),
        "{label} admitted Wasm releases all roots");
}

fn assert_refused(label: &str, source: &str, needle: &str) {
    let error = witchy::resolve_std_only_checked(source).err().map(|error| error.to_string())
        .unwrap_or_else(|| panic!("{label} is refused at checking"));
    assert!(error.contains(needle), "{label} refusal names the rule: {error}");
}

const GENERATORS: &str = "GEN_SOURCE";

const CHANNELS: &str = "CHAN_SOURCE";

#[test]
fn generators_take_and_yield_views() {
    assert_output("generator views", GENERATORS, &[]);
}

#[test]
fn channels_carry_views_between_tasks() {
    assert_output("channel views", CHANNELS, &[]);
}

#[test]
fn a_region_born_view_cannot_leave_in_a_generator_or_a_send() {
    assert_refused("generator", "REGION1", "region-born root");
    assert_refused("send", "REGION2", "region-born root");
}
