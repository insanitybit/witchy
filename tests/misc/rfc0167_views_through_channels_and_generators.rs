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

const GENERATORS: &str = r#"
import iter

gen fn values(v: &Int) -> Iter(&Int):
    yield v

fn main(console: Console):
    var n = 21
    let sequence = values(&n)
    n = 5
    match iter.next(sequence):
        Item(value, _) -> console.print("${*value} ${n}")
        Empty -> console.print("unexpected empty")
"#;

const CHANNELS: &str = r#"
from chan import Sender

async fn producer(tx: Sender(&Int), v: &Int):
    chan.send(tx, v).await

async fn main(console: Console):
    var n = 21
    let (tx, rx) = chan.channel(1).await
    let child = chan.spawn(producer(tx, &n)).await
    n = 5
    let received: Option(&Int) = chan.recv(rx).await
    match received:
        Some(value) -> console.print("${*value} ${n}")
        None -> console.print("unexpected closed")
    chan.join(child).await
"#;

#[test]
fn generators_take_and_yield_views() {
    assert_output("generator views", GENERATORS, &["21 5"]);
}

#[test]
fn channels_carry_views_between_tasks() {
    assert_output("channel views", CHANNELS, &["21 5"]);
}

#[test]
fn a_region_born_view_cannot_leave_in_a_generator_or_a_send() {
    assert_refused("generator", r#"
import iter

gen fn values(v: &List(Int)) -> Iter(&List(Int)):
    yield v

fn main(console: Console):
    let sequence = region -> Iter(&List(Int)):
        let local = [1, 2]
        values(&local)
    let _ = iter.next(sequence)
"#, "region-born root");
    assert_refused("send", r#"
from chan import Sender
from task import Task

async fn producer(tx: Sender(&List(Int)), v: &List(Int)):
    chan.send(tx, v).await

async fn main(console: Console):
    let (tx, _rx) = chan.channel(1).await
    let recipe = region -> Task(()):
        let local = [1, 2]
        producer(tx, &local)
    let child = chan.spawn(recipe).await
    chan.join(child).await
"#, "region-born root");
}
