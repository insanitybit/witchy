use witchy::opt::{OptSet, set_for_tests};
use witchy::runtime::{Capabilities, Runtime};

const SOURCE: &str = r#"from chan import Sender, Receiver
from task import Task

fn submit(let tx: Sender(Int), value: Int) -> Task(()):
    chan.send(tx, value)

fn receive(let rx: Receiver(Int)) -> Task(Option(Int)):
    chan.recv(rx)

async fn main(console: Console):
    let (tx, rx) = chan.channel(0).await
    let alias = tx
    let reader = rx
    let later = submit(alias, 7)
    submit(tx, -3).await
    later.await
    console.print("${receive(reader).await} ${receive(rx).await}")
    let (other_tx, other_rx) = chan.channel(0).await
    submit(other_tx, 11).await
    submit(tx, 13).await
    console.print("${receive(other_rx).await} ${receive(rx).await}")
    let (text_tx, text_rx) = chan.channel(0).await
    let text_alias = text_tx
    chan.send(text_alias, "first").await
    chan.send(text_tx, "second").await
    match chan.recv(text_rx).await:
        Some(text) -> console.print(text)
        None -> console.print("missing first String message")
    match chan.recv(text_rx).await:
        Some(text) -> console.print(text)
        None -> console.print("missing second String message")
    let (values_tx, values_rx) = chan.channel(0).await
    var values = [4, 5]
    let queued = chan.send(values_tx, values)
    values[0] = 9
    queued.await
    match chan.recv(values_rx).await:
        Some(snapshot) -> console.print("${snapshot[0]} ${snapshot[1]} ${values[0]}")
        None -> console.print("missing List message snapshot")
"#;

#[test]
fn endpoint_aliases_share_channel_identity_and_tasks_own_message_snapshots() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { set_for_tests(None); } }
    let _reset = Reset;
    for options in [OptSet::none(), OptSet::all()] {
        set_for_tests(Some(options));
        let checked = witchy::resolve_std_only_checked(SOURCE).expect("check borrowed channel helpers");
        let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("admit endpoint clones and borrowed helpers");
        let mut runtime = Runtime::batch().unwrap();
        let mut actor = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, quiet: true, ..Capabilities::none() }, 256).unwrap();
        let expected = ["Some(-3) Some(7)", "Some(11) Some(13)", "first", "second", "4 5 9"];
        for replay in 0..2 {
            actor.run().unwrap();
            assert_eq!(actor.output(), expected.repeat(replay + 1));
            assert_eq!(actor.canonical_heap_leaks().unwrap(), Some(0));
        }
    }
}
