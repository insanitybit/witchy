//! RFC-0167 row K channel boundary. A live `must` child handle cannot enter
//! the replayable Task continuation installed by `await`.

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
    let refusal = match witchy::resolve_std_only_checked(CHANNELS) {
        Err(error) => error,
        Ok(_) => panic!("live child handle must not enter a replayable continuation"),
    };
    assert!(refusal.to_string().contains("closure") && refusal.to_string().contains("capture"),
        "unexpected refusal: {refusal}");
}
