use witchy::opt::{Opt, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct Reset;
impl Drop for Reset { fn drop(&mut self) { witchy::opt::set_for_tests(None); } }

const TRACE: &str = r#"from chan import Sender, Receiver

async fn produce(emit: fn(String) -> (), completed: fn() -> (), tx: Sender(Int)):
    var i: Int = 0
    while i < 3:
        emit("before ${i}")
        chan.send(tx, i).await
        emit("after ${i}")
        i = i + 1
    completed()

async fn consume(emit: fn(String) -> (), rx: Receiver(Int)):
    for await value in rx:
        emit("value ${value}")

async fn main(console: Console):
    let (tx, rx) = chan.channel(1).await
    let emit = fn(text: String): console.print(text)
    let completed = fn(): console.print("sent")
    chan.scope([produce(emit, completed, tx), consume(emit, rx)]).await
    console.print("joined")
"#;

fn run(source: &str, options: OptSet) -> Vec<String> {
    witchy::opt::set_for_tests(Some(options));
    let checked = witchy::resolve_std_only_checked(source)
        .unwrap_or_else(|error| panic!("check producer frame fixture with {options:?}: {error}"));
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admit producer frame fixture");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print: true, print_int: true, quiet: true, ..Capabilities::none() }, 256)
        .expect("spawn admitted producer frame fixture");
    vm.run().expect("execute producer frame fixture");
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
    vm.output()
}

#[test]
fn producer_frame_keeps_prefix_suffix_and_probe_rounds() {
    let _reset = Reset;
    for capacity in [0, 1, 3] {
        let source = TRACE.replace("chan.channel(1)", &format!("chan.channel({capacity})"));
        let reference = run(&source, OptSet::none());
        let values = reference.iter().filter(|line| line.starts_with("value ")).cloned().collect::<Vec<_>>();
        assert_eq!(values, ["value 0", "value 1", "value 2"]);
        assert_eq!(run(&source, OptSet::all()), reference);
        assert_eq!(run(&source, OptSet::all().without(Opt::ClosureElide)), reference);
    }
}

#[test]
fn producer_frame_empty_loop_preserves_initial_completion_round() {
    let _reset = Reset;
    let source = TRACE.replace("i < 3", "i < 0");
    let reference = run(&source, OptSet::none());
    assert!(reference.contains(&"sent".to_string()));
    assert!(!reference.iter().any(|line| line.starts_with("value ")));
    assert_eq!(run(&source, OptSet::all()), reference);
}

#[test]
fn producer_frame_replay_has_independent_carried_state() {
    let _reset = Reset;
    let source = r#"from chan import Sender

async fn produce(tx: Sender(Int)):
    var i: Int = 0
    while i < 2:
        chan.send(tx, i).await
        i = i + 1
    ()

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    let recipe = produce(tx)
    recipe.await
    recipe.await
    for await value in rx:
        console.print("${value}")
"#;
    for options in [OptSet::none(), OptSet::all()] {
        assert_eq!(run(source, options), ["0", "1", "0", "1"]);
    }
}

#[test]
fn producer_frame_preserves_nominal_payload_and_bool_carry() {
    let _reset = Reset;
    let source = TRACE.replace("from chan import Sender, Receiver", "from chan import Sender, Receiver\n\ntype Packet:\n    Packet(Int, Bool)")
        .replace("Sender(Int)", "Sender(Packet)").replace("Receiver(Int)", "Receiver(Packet)")
        .replace("    var i: Int = 0", "    var i: Int = 0\n    var valid: Bool = false")
        .replace("chan.send(tx, i)", "chan.send(tx, Packet(i, valid))")
        .replace("        i = i + 1", "        i = i + 1\n        valid = !valid")
        .replace("        emit(\"value ${value}\")", "        match value:\n            Packet(i, valid) -> emit(\"value ${i} ${valid}\")");
    let reference = run(&source, OptSet::none());
    let values = reference.iter().filter(|line| line.starts_with("value ")).cloned().collect::<Vec<_>>();
    assert_eq!(values, ["value 0 false", "value 1 true", "value 2 false"]);
    assert_eq!(run(&source, OptSet::all()), reference);
}

#[test]
fn producer_frame_cancellation_releases_once_prepared_string_payload() {
    let _reset = Reset;
    let source = r#"from chan import Sender
import task

async fn produce(emit: fn(Int) -> (), tx: Sender(String)) -> Int:
    var i: Int = 0
    while i < 5:
        emit(i)
        chan.send(tx, "payload ${i}").await
        i = i + 1
    9

async fn main(console: Console):
    let (tx, rx) = chan.channel(1).await
    chan.send(tx, "seed").await
    let result = chan.race(produce(fn(value: Int): console.print("before ${value}"), tx), task.done(-1)).await
    console.print("winner ${result}")
    for await value in rx:
        console.print(value)
"#;
    let reference = run(source, OptSet::none());
    assert_eq!(reference, ["before 0", "winner Some(-1)", "seed"]);
    assert_eq!(run(source, OptSet::all()), reference);
}

#[test]
fn raw_producer_preparation_is_not_source_authority() {
    for source in [
        "import task\n\nfn main():\n    let _ = task.__prepare_send(task.ready_unit())\n",
        "import task\n\nfn main():\n    let _ = task.__producer_frame([], fn(var state): None, fn(var state): false, fn(state): task.ready_unit())\n",
    ] {
        let error = witchy::resolve_std_only_checked(source).expect_err("private producer bridge");
        assert!(error.to_string().contains("private"), "{error}");
    }
}
