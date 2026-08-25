//! RFC-0152 Phase 0: a compact channel schedule matrix must retain the same
//! observable result through the interpreter, ordinary Wasm, and `mode opt`
//! Wasm.  The cases intentionally cover distinct scheduler boundaries rather
//! than re-testing one producer/consumer shape many times.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};

struct ChannelCase {
    label: &'static str,
    source: &'static str,
    expected: &'static [&'static str],
}

const CASES: &[ChannelCase] = &[
    ChannelCase {
        label: "ready select4 source order",
        source: r#"
async fn main(console: Console):
    let (a_tx, a_rx) = chan.channel(8).await
    let (b_tx, b_rx) = chan.channel(8).await
    let (c_tx, c_rx) = chan.channel(8).await
    let (d_tx, d_rx) = chan.channel(8).await
    chan.send(c_tx, 30).await
    chan.send(d_tx, 40).await
    let selected = chan.select4(a_rx, b_rx, c_rx, d_rx).await
    match selected:
        First4(value) -> console.print("wrong first ${value}")
        Second4(value) -> console.print("wrong second ${value}")
        Third4(value) -> console.print("ready ${value}")
        Fourth4(value) -> console.print("wrong fourth ${value}")
        Closed4 -> console.print("closed too early")
"#,
        expected: &["ready 30"],
    },
    ChannelCase {
        label: "capacity zero quiescence and blocked producer wake",
        source: r#"
from chan import Sender

async fn producer(console: Console, tx: Sender(Int)):
    chan.send(tx, 41).await
    console.print("producer queued first")
    chan.send(tx, 42).await
    console.print("producer resumed")

async fn main(console: Console):
    let (quiet_tx, quiet_rx) = chan.channel(0).await
    let quiet_before = chan.recv(quiet_rx).await
    chan.send(quiet_tx, 5).await
    let quiet_after = chan.recv(quiet_rx).await
    console.print("capacity-zero ${quiet_before} ${quiet_after}")

    let (tx, rx) = chan.channel(1).await
    let worker = chan.spawn(producer(console, tx)).await
    chan.yield_now().await
    let first = chan.recv(rx).await
    console.print("first ${first}")
    let second = chan.recv(rx).await
    console.print("second ${second}")
    chan.join(worker).await
"#,
        expected: &[
            "capacity-zero None Some(5)",
            "producer queued first",
            "first Some(41)",
            "producer resumed",
            "second Some(42)",
        ],
    },
    ChannelCase {
        label: "buffered string survives cancellation then closes",
        source: r#"
from chan import Sender

async fn producer(tx: Sender(String)):
    chan.send(tx, "first").await
    chan.yield_now().await
    chan.send(tx, "second").await

async fn main(console: Console):
    let (tx, rx) = chan.channel(1).await
    let worker = chan.spawn(producer(tx)).await
    chan.yield_now().await
    chan.cancel(worker).await
    let first = chan.recv(rx).await
    let closed = chan.recv(rx).await
    console.print("first ${first} closed ${closed}")
"#,
        expected: &["first Some(first) closed None"],
    },
    ChannelCase {
        label: "large capacity typed record payload",
        source: r#"
from chan import Sender

type Packet:
    Packet(Int, String)

async fn producer(tx: Sender(Packet)):
    chan.send(tx, Packet(7, "seven")).await
    chan.send(tx, Packet(9, "nine")).await

async fn main(console: Console):
    let (tx, rx) = chan.channel(2048).await
    let worker = chan.spawn(producer(tx)).await
    let first = chan.recv(rx).await
    let second = chan.recv(rx).await
    chan.join(worker).await
    match (first, second):
        (Some(Packet(a, left)), Some(Packet(b, right))) ->
            console.print("packets ${a}:${left} ${b}:${right}")
        _ -> fail("missing typed packet")
"#,
        expected: &["packets 7:seven 9:nine"],
    },
];

fn run_interpreted(source: &str, label: &str) -> Vec<String> {
    let checked = witchy::resolve_std_only_checked(source)
        .unwrap_or_else(|error| panic!("{label}: source must check: {error}"));
    interpreter::run_checked_module(&checked, ".", Vec::new())
        .unwrap_or_else(|error| panic!("{label}: interpreter must run: {error}"))
}

fn run_compiled(source: &str, label: &str) -> Vec<String> {
    let checked = witchy::resolve_std_only_checked(source)
        .unwrap_or_else(|error| panic!("{label}: source must check: {error}"));
    let wasm = codegen::compile_checked_module_binary(&checked)
        .expect_lowered(&format!("{label}: compile channel schedule"));
    let mut runtime = Runtime::batch().unwrap_or_else(|error| panic!("{label}: create runtime: {error}"));
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities {
                print: true,
                quiet: true,
                ..Default::default()
            },
            256,
        )
        .unwrap_or_else(|error| panic!("{label}: spawn compiled Wasm: {error}"));
    actor.run().unwrap_or_else(|error| panic!("{label}: run compiled Wasm: {error}"));
    actor.output()
}

fn assert_case(case: &ChannelCase) {
    let ordinary_interpreted = run_interpreted(case.source, case.label);
    assert_eq!(ordinary_interpreted, case.expected, "{}: independent expected result", case.label);

    let ordinary_compiled = run_compiled(case.source, case.label);
    assert_eq!(ordinary_compiled, ordinary_interpreted, "{}: ordinary Wasm parity", case.label);

    let optimized_source = format!("mode opt\n\n{}", case.source);
    let optimized_interpreted = run_interpreted(&optimized_source, case.label);
    assert_eq!(optimized_interpreted, case.expected, "{}: opt interpreter result", case.label);

    let optimized_compiled = run_compiled(&optimized_source, case.label);
    assert_eq!(optimized_compiled, optimized_interpreted, "{}: optimized Wasm parity", case.label);
    assert_eq!(optimized_compiled, ordinary_compiled, "{}: ordinary and optimized Wasm agree", case.label);
}

#[test]
fn rfc0152_phase0_ready_select4_matrix() {
    assert_case(&CASES[0]);
}

#[test]
fn rfc0152_phase0_quiescence_and_blocked_wake_matrix() {
    assert_case(&CASES[1]);
}

#[test]
fn rfc0152_phase0_cancelled_string_matrix() {
    assert_case(&CASES[2]);
}

#[test]
fn rfc0152_phase0_large_record_payload_matrix() {
    assert_case(&CASES[3]);
}
