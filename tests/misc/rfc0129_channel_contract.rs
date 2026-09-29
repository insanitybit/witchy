//! RFC-0129 channels on admitted Wasm and the normal-mode handle boundary.

use witchy::runtime::{Capabilities, Runtime};
use witchy::codegen;

const CHANNEL_CONTRACT: &str = r#"
from chan import Receiver, Selected, Selected3, Selected4, Sender

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

    let (first3_tx, first3_rx) = chan.channel(1).await
    let (second3_tx, second3_rx) = chan.channel(1).await
    let (third3_tx, third3_rx) = chan.channel(1).await
    chan.send(first3_tx, 30).await
    chan.send(second3_tx, 31).await
    chan.send(third3_tx, 32).await
    let selected3 = chan.select3(first3_rx, second3_rx, third3_rx).await
    match selected3:
        First3(value) -> console.print("select3 first ${value}")
        Second3(value) -> console.print("select3 wrong ${value}")
        Third3(value) -> console.print("select3 wrong ${value}")
        Closed3 -> console.print("select3 closed too early")

    let (first4_tx, first4_rx) = chan.channel(1).await
    let (second4_tx, second4_rx) = chan.channel(1).await
    let (third4_tx, third4_rx) = chan.channel(1).await
    let (fourth4_tx, fourth4_rx) = chan.channel(1).await
    chan.send(third4_tx, 43).await
    chan.send(fourth4_tx, 44).await
    let selected4 = chan.select4(first4_rx, second4_rx, third4_rx, fourth4_rx).await
    match selected4:
        First4(value) -> console.print("select4 wrong ${value}")
        Second4(value) -> console.print("select4 wrong ${value}")
        Third4(value) -> console.print("select4 third ${value}")
        Fourth4(value) -> console.print("select4 wrong ${value}")
        Closed4 -> console.print("select4 closed too early")

    let (_cancel_tx, cancel_rx) = chan.channel(1).await
    let doomed = chan.spawn(cancelled_child(console, cancel_rx)).await
    chan.yield_now().await
    chan.cancel(doomed).await
    chan.yield_now().await
    console.print("cancelled")

    chan.scope([scoped_child(console, "A"), scoped_child(console, "B")]).await
    console.print("scope joined")
"#;

const BASIC_CHANNEL: &str = r#"
async fn main(console: Console):
    let (tx, rx) = chan.channel(1).await
    chan.send(tx, 7).await
    let value = chan.recv(rx).await
    console.print("channel ${value}")
"#;

fn run_compiled(source: &str) -> Vec<String> {
    let checked = witchy::resolve_std_only_checked(source)
        .expect("RFC-0129 row-2 channel contract must check");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admit RFC-0129 row-2 channel contract");
    let mut runtime = Runtime::batch().expect("create RFC-0129 row-2 runtime");
    let mut actor = runtime
        .spawn_admitted(
            &artifact,
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
fn rfc0129_spawned_handle_refuses_unproved_suspension_disposition() {
    let error = witchy::resolve_std_only_checked(CHANNEL_CONTRACT)
        .expect_err("a live must-consume producer handle cannot cross suspension");
    let message = error.to_string();
    assert!(
        message.contains("cannot `await` while must-consume value `producer` is live")
            && message.contains("cancellation at this suspension has no checked disposition path"),
        "unexpected handle refusal: {message}",
    );
}

#[test]
fn rfc0129_basic_typed_channel_runs_on_admitted_wasm() {
    assert_eq!(run_compiled(BASIC_CHANNEL), ["channel Some(7)"]);
}
