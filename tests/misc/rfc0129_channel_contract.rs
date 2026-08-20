//! RFC-0129 row 2: the typed channel surface has one deterministic structured
//! concurrency contract on the interpreter and compiled Wasm.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};

const CHANNEL_CONTRACT: &str = r#"
from chan import Receiver, Selected, Sender

async fn bounded_producer(console: Console, tx: Sender(Int)):
    console.print("producer before 1")
    chan.send(tx, 1).await
    console.print("producer after 1")
    console.print("producer before 2")
    chan.send(tx, 2).await
    console.print("producer after 2")

async fn text_producer(tx: Sender(String)):
    chan.send(tx, "typed text").await

async fn cancelled_child(console: Console, rx: Receiver(Int)):
    console.print("cancel started")
    let value = chan.recv(rx).await
    console.print("cancel failed ${value}")

async fn scoped_child(console: Console, name: String):
    console.print("scope ${name} start")
    chan.yield_now().await
    console.print("scope ${name} done")

async fn main(console: Console):
    let (int_tx, int_rx) = chan.channel(1).await
    let producer = chan.spawn(bounded_producer(console, int_tx)).await
    chan.yield_now().await
    console.print("consumer before 1")
    let first = chan.recv(int_rx).await
    console.print("consumer ${first}")
    let second = chan.recv(int_rx).await
    console.print("consumer ${second}")
    chan.join(producer).await

    let (text_tx, text_rx) = chan.channel(1).await
    let text_handle = chan.spawn(text_producer(text_tx)).await
    let text = chan.recv(text_rx).await
    console.print("text ${text}")
    chan.join(text_handle).await

    let (left_tx, left_rx) = chan.channel(1).await
    let (right_tx, right_rx) = chan.channel(1).await
    chan.send(left_tx, 10).await
    chan.send(right_tx, 20).await
    let selected_first = chan.select(left_rx, right_rx).await
    match selected_first:
        First(value) -> console.print("select first ${value}")
        Second(value) -> console.print("select wrong ${value}")
        Closed -> console.print("select closed too early")
    let selected_second = chan.select(left_rx, right_rx).await
    match selected_second:
        First(value) -> console.print("select wrong ${value}")
        Second(value) -> console.print("select second ${value}")
        Closed -> console.print("select closed too early")
    let selected_closed = chan.select(left_rx, right_rx).await
    match selected_closed:
        First(value) -> console.print("select wrong ${value}")
        Second(value) -> console.print("select wrong ${value}")
        Closed -> console.print("select closed")

    let (_cancel_tx, cancel_rx) = chan.channel(1).await
    let doomed = chan.spawn(cancelled_child(console, cancel_rx)).await
    chan.yield_now().await
    chan.cancel(doomed).await
    chan.yield_now().await
    console.print("cancelled")

    chan.scope([scoped_child(console, "A"), scoped_child(console, "B")]).await
    console.print("scope joined")
"#;

fn run_compiled(source: &str) -> Vec<String> {
    let checked = witchy::resolve_std_only_checked(source)
        .expect("RFC-0129 row-2 channel contract must check");
    let wasm = codegen::compile_checked_module_binary(&checked)
        .expect_lowered("compile RFC-0129 row-2 channel contract");
    let mut runtime = Runtime::batch().expect("create RFC-0129 row-2 runtime");
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
        .expect("spawn RFC-0129 row-2 channel contract");
    actor.run().expect("run RFC-0129 row-2 compiled Wasm");
    actor.output()
}

#[test]
fn rfc0129_acceptance_row_2_typed_bounded_selected_cancelled_and_joined_channels_agree() {
    let checked = witchy::resolve_std_only_checked(CHANNEL_CONTRACT)
        .expect("RFC-0129 row-2 channel contract must check");
    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("run RFC-0129 row-2 channel contract on the interpreter");
    let compiled = run_compiled(CHANNEL_CONTRACT);
    assert_eq!(compiled, interpreted, "row-2 channel schedule must have backend parity");

    assert_eq!(
        interpreted,
        [
            "producer before 1",
            "producer after 1",
            "producer before 2",
            "consumer before 1",
            "consumer Some(1)",
            "producer after 2",
            "consumer Some(2)",
            "text Some(typed text)",
            "select first 10",
            "select second 20",
            "select closed",
            "cancel started",
            "cancelled",
            "scope A start",
            "scope A done",
            "scope B start",
            "scope B done",
            "scope joined",
        ],
        "capacity one must park the second send, selection must prefer the first ready receiver, cancellation must stop the child, and scope must join every child",
    );
}
