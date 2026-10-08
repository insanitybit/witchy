use witchy::opt::{Opt, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct Reset;
impl Drop for Reset { fn drop(&mut self) { witchy::opt::set_for_tests(None); } }

const TRACE: &str = r#"from chan import Receiver, Sender

async fn produce(emit: fn(String) -> (), completed: fn() -> (), tx: Sender(Int)):
    var i = 0
    while i < 4:
        emit("send ${i}")
        chan.send(tx, i).await
        i = i + 1
    completed()

async fn consume(emit: fn(String) -> (), completed: fn(Int, Int) -> (), rx: Receiver(Int)):
    var seen = 0
    var values: List(Int) = [0, 0]
    while seen < 4:
        emit("prepare ${seen}")
        match chan.recv(rx).await:
            Some(value) ->
                list.set_at(values, seen % 2, value)
                seen = seen + 1
                emit("receive ${value} ${values[0]} ${values[1]}")
            None -> seen = 4
    completed(values[0], values[1])

async fn main(console: Console):
    let (tx, rx) = chan.channel(1).await
    let emit = fn(text: String): console.print(text)
    let producer_done = fn(): console.print("producer done")
    let receiver_done = fn(first: Int, second: Int): console.print("receiver done ${first} ${second}")
    chan.scope([produce(emit, producer_done, tx), consume(emit, receiver_done, rx)]).await
    console.print("joined")
"#;

fn selected(source: &str) -> bool {
    let checked = witchy::resolve_std_only_checked(source).expect("checked receive state selection");
    let mut found = false;
    witchy_syntax::ast::visit::visit_module_exprs(checked.module(), &mut |expression| {
        if let witchy_syntax::ast::Expr::Call { name, .. } = expression { found |= name == "chan.__recv_state_frame"; }
        Ok::<_, ()>(())
    }).unwrap();
    found
}

fn run(source: &str, options: OptSet) -> Vec<String> {
    witchy::opt::set_for_tests(Some(options));
    let checked = witchy::resolve_std_only_checked(source).expect("check state frame");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked).expect_lowered("admit state frame");
    let mut runtime = Runtime::batch().unwrap();
    let mut output = None;
    for _ in 0..2 {
        let mut vm = runtime.spawn_admitted(&artifact, Capabilities { print: true, quiet: true, ..Capabilities::none() }, 256).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
        let current = vm.output();
        if let Some(previous) = &output { assert_eq!(previous, &current); }
        output = Some(current);
    }
    output.unwrap()
}

fn preserve_rounds_and_list_value_semantics(capacity: i64) {
    let _reset = Reset;
    let source = TRACE.replace("channel(1)", &format!("channel({capacity})"));
    let expected = if capacity == 1 {
        vec!["send 0", "send 1", "prepare 0", "receive 0 0 0", "prepare 1", "send 2",
            "receive 1 0 1", "prepare 2", "send 3", "receive 2 2 1", "prepare 3",
            "producer done", "receive 3 2 3", "receiver done 2 3", "joined"]
    } else {
        vec!["send 0", "send 1", "prepare 0", "send 2", "receive 0 0 0", "prepare 1",
            "send 3", "receive 1 0 1", "prepare 2", "receive 2 2 1", "prepare 3",
            "producer done", "receive 3 2 3", "receiver done 2 3", "joined"]
    }.into_iter().map(str::to_string).collect::<Vec<_>>();
    assert_eq!(run(&source, OptSet::none()), expected);
    witchy::opt::set_for_tests(Some(OptSet::all()));
    assert!(selected(&source));
    assert_eq!(run(&source, OptSet::all()), expected);
    assert_eq!(run(&source, OptSet::all().without(Opt::ClosureElide)), expected);
}

#[test]
fn async_receive_state_frames_preserve_rounds_and_list_value_semantics_unbounded() {
    preserve_rounds_and_list_value_semantics(-1);
}

#[test]
fn async_receive_state_frames_preserve_rounds_and_list_value_semantics_rendezvous() {
    preserve_rounds_and_list_value_semantics(0);
}

#[test]
fn async_receive_state_frames_preserve_rounds_and_list_value_semantics_single_slot() {
    preserve_rounds_and_list_value_semantics(1);
}

#[test]
fn async_receive_state_frames_preserve_rounds_and_list_value_semantics_buffered() {
    preserve_rounds_and_list_value_semantics(4);
}
#[test]
fn async_receive_state_frames_replay_and_snapshot() {
    let _reset = Reset;
    let source = r#"from chan import Receiver

async fn consume(rx: Receiver(Int)) -> Int:
    var seen = 0
    var total = 0
    var values: List(Int) = [10]
    let before = values
    while seen < 2:
        match chan.recv(rx).await:
            Some(value) ->
                list.set_at(values, 0, value)
                total = total + value
                seen = seen + 1
            None -> seen = 2
    total + before[0] + values[0]

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    chan.send(tx, 1).await
    chan.send(tx, 2).await
    let recipe = consume(rx)
    let first = recipe.await
    chan.send(tx, 1).await
    chan.send(tx, 2).await
    let second = recipe.await
    console.print("${first} ${second}")
"#;
    let expected = run(source, OptSet::none());
    assert_eq!(expected, ["15 15"]);
    witchy::opt::set_for_tests(Some(OptSet::all()));
    assert!(selected(source));
    assert_eq!(run(source, OptSet::all()), expected);
}
