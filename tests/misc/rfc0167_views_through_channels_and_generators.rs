//! RFC-0167 row K channel boundary, still awaiting a consuming GC task frame.
//! A live `must` child handle cannot be captured across the receive await:
//! cancellation would otherwise abandon its join obligation. This program
//! stays fail-closed until one-shot capture and cancellation are implemented.

use witchy::codegen;

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
fn channels_with_live_child_handle_fail_closed() {
    let checked = witchy::resolve_std_only_checked(CHANNELS).expect("channel source checks");
    let refusal = codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
        .expect_unsupported("live child handle must not cross a cancellable receive");
    assert!(refusal.to_string().contains("closure") || refusal.to_string().contains("capture"),
        "unexpected refusal: {refusal}");
}
