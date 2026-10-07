use witchy::opt::{Opt, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct Reset;
impl Drop for Reset { fn drop(&mut self) { witchy::opt::set_for_tests(None); } }

const TRACE: &str = r#"from chan import Receiver, Sender

async fn produce(emit: fn(String) -> (), tx: Sender(Int), start: Int):
    var index: Int = 0
    while index < 2:
        emit("send ${start + index}")
        chan.send(tx, start + index).await
        index = index + 1
    ()

async fn consume(emit: fn(String) -> (), completed: fn(Int) -> (), a: Receiver(Int), b: Receiver(Int)):
    var total: Int = 0
    var alive: Bool = true
    while alive:
        emit("before ${total}")
        let selected = chan.select(a, b).await
        match selected:
            First(value) ->
                total = total + value
                emit("first ${value}")
            Second(value) ->
                total = total + value
                emit("second ${value}")
            Closed -> alive = false
        emit("after ${total}")
    completed(total)

async fn main(console: Console):
    let (first_tx, first_rx) = chan.channel(1).await
    let (second_tx, second_rx) = chan.channel(1).await
    let emit = fn(text: String): console.print(text)
    let completed = fn(total: Int): console.print("done ${total}")
    chan.scope([produce(emit, first_tx, 10), produce(emit, second_tx, 20), consume(emit, completed, first_rx, second_rx)]).await
    console.print("joined")
"#;

const REPLAY: &str = r#"from chan import Receiver

async fn consume(a: Receiver(Int), b: Receiver(Int)) -> Int:
    var total: Int = 0
    var alive: Bool = true
    while alive:
        let selected = chan.select(a, b).await
        match selected:
            First(value) -> total = total + value
            Second(value) -> total = total + value
            Closed -> alive = false
    total

async fn main(console: Console):
    let (first_tx, first_rx) = chan.channel(4).await
    let (second_tx, second_rx) = chan.channel(4).await
    chan.send(first_tx, 1).await
    chan.send(second_tx, 9).await
    chan.send(first_tx, 2).await
    let recipe = consume(first_rx, second_rx)
    let first = recipe.await
    let second = recipe.await
    console.print("${first} ${second}")
"#;

fn attempt(source: &str, options: OptSet) -> Result<Vec<String>, String> {
    witchy::opt::set_for_tests(Some(options));
    let checked = witchy::resolve_std_only_checked(source)
        .unwrap_or_else(|error| panic!("check select lane fixture with {options:?}: {error}"));
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admit select lane fixture");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut previous = None;
    for _ in 0..2 {
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, quiet: true, ..Capabilities::none() }, 256)
            .expect("spawn admitted select lane fixture");
        vm.run().map_err(|error| error.root_cause().to_string())?;
        assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
        let output = vm.output();
        if let Some(previous) = &previous { assert_eq!(&output, previous); }
        previous = Some(output);
    }
    Ok(previous.expect("executed fixture"))
}

fn run(source: &str, options: OptSet) -> Vec<String> {
    attempt(source, options).expect("execute select lane fixture")
}

fn selected(source: &str) -> bool {
    let checked = witchy::resolve_std_only_checked(source).expect("checked selection fixture");
    let mut selected = false;
    witchy_syntax::ast::visit::visit_module_exprs(checked.module(), &mut |expression| {
        if let witchy_syntax::ast::Expr::Call { name, .. } = expression {
            selected |= name == "chan.__select2_lane_frame";
        }
        Ok::<_, ()>(())
    }).expect("walk selected calls");
    selected
}

#[test]
fn select_lane_keeps_source_order_body_prepare_and_probe_rounds() {
    let _reset = Reset;
    for capacity in [0, 1, 4] {
        let source = TRACE.replace("chan.channel(1)", &format!("chan.channel({capacity})"));
        let reference = run(&source, OptSet::none());
        let mut payloads = reference.iter().filter(|line| line.starts_with("first ") || line.starts_with("second "))
            .cloned().collect::<Vec<_>>();
        payloads.sort();
        assert_eq!(payloads, ["first 10", "first 11", "second 20", "second 21"]);
        assert!(reference.contains(&"done 62".to_string()));
        assert_eq!(run(&source, OptSet::all()), reference);
        assert!(selected(&source));
        assert_eq!(run(&source, OptSet::all().without(Opt::ClosureElide)), reference);
        assert!(!selected(&source));
    }
}

#[test]
fn select_lane_recipe_replay_has_fresh_scalar_carries() {
    let _reset = Reset;
    for options in [OptSet::none(), OptSet::all()] {
        assert_eq!(run(REPLAY, options), ["12 0"]);
        assert_eq!(selected(REPLAY), options == OptSet::all());
    }
}

#[test]
fn select_lane_empty_initial_condition_keeps_completion_round() {
    let _reset = Reset;
    let source = TRACE.replace("var alive: Bool = true", "var alive: Bool = false");
    let reference = run(&source, OptSet::none());
    assert!(reference.contains(&"done 0".to_string()));
    assert!(!reference.iter().any(|line| line.starts_with("before ")));
    assert_eq!(run(&source, OptSet::all()), reference);
}

#[test]
fn select_lane_keeps_reference_payloads_and_ordinary_channel_use() {
    let _reset = Reset;
    let source = REPLAY.replace("Receiver(Int)", "Receiver(String)")
        .replace("total = total + value", "let text: String = value\n                total = total + text.length()")
        .replace("First(value) -> let", "First(value) ->\n                let")
        .replace("Second(value) -> let", "Second(value) ->\n                let")
        .replace("chan.send(first_tx, 1)", "chan.send(first_tx, \"ab\")")
        .replace("chan.send(second_tx, 9)", "chan.send(second_tx, \"xyz\")")
        .replace("chan.send(first_tx, 2)", "chan.send(first_tx, \"q\")");
    for options in [OptSet::none(), OptSet::all()] {
        assert_eq!(run(&source, options), ["6 0"]);
        assert_eq!(selected(&source), options == OptSet::all());
    }
}

#[test]
fn select_lane_keeps_pooled_and_ordinary_reference_channel_use() {
    let _reset = Reset;
    let source = TRACE.replace("Sender(Int)", "Sender(String)")
        .replace("Receiver(Int)", "Receiver(String)")
        .replace("chan.channel(1)", "chan.channel(4)")
        .replace("chan.send(tx, start + index)", "chan.send(tx, \"${start + index}\")")
        .replace("total = total + value", "let text: String = value\n                total = total + text.length()")
        .replace("    let emit =", "    chan.send(first_tx, \"seed\").await\n    let emit =");
    let reference = run(&source, OptSet::none());
    assert!(reference.contains(&"first seed".to_string()));
    assert!(reference.contains(&"done 12".to_string()));
    assert_eq!(run(&source, OptSet::all()), reference);
    assert!(selected(&source));
}

#[test]
fn select_lane_cancellation_releases_pending_string_payloads() {
    let _reset = Reset;
    let source = r#"from chan import Receiver
import task

async fn consume(a: Receiver(String), b: Receiver(String)) -> Int:
    var total: Int = 0
    var alive: Bool = true
    while alive:
        let selected = chan.select(a, b).await
        match selected:
            First(value) ->
                let text: String = value
                total = total + text.length()
            Second(value) ->
                let text: String = value
                total = total + text.length()
            Closed -> alive = false
    total

async fn main(console: Console):
    let (first_tx, first_rx) = chan.channel(4).await
    let (second_tx, second_rx) = chan.channel(4).await
    chan.send(first_tx, "payload").await
    chan.send(second_tx, "other").await
    let winner = chan.race(consume(first_rx, second_rx), task.done(-1)).await
    console.print("${winner}")
    chan.yield_now().await
    console.print("end")
"#;
    let reference = run(source, OptSet::none());
    assert_eq!(reference, ["Some(-1)", "end"]);
    assert_eq!(run(source, OptSet::all()), reference);
    assert!(selected(source));
}

#[test]
fn select_lane_keeps_prepare_and_resume_trap_locations() {
    let _reset = Reset;
    for source in [
        REPLAY.replace("        let selected =", "        let bad: Int = 42 / (total - total)\n        let _ = bad\n        let selected ="),
        REPLAY.replace("First(value) -> total = total + value", "First(value) -> total = 42 / (value - value)"),
    ] {
        let reference = attempt(&source, OptSet::none()).expect_err("division must trap");
        assert!(reference.contains("division by zero"), "{reference}");
        assert_eq!(attempt(&source, OptSet::all()).expect_err("select lane division must trap"), reference);
    }
}

#[test]
fn select_lane_keeps_capability_message_payloads_fail_closed() {
    let _reset = Reset;
    let source = r#"from chan import Receiver

async fn consume(a: Receiver(Console), b: Receiver(Console)) -> Int:
    var count: Int = 0
    var alive: Bool = true
    while alive:
        let selected = chan.select(a, b).await
        match selected:
            First(value) ->
                let output: Console = value
                output.print("first")
                count = count + 1
            Second(value) ->
                let output: Console = value
                output.print("second")
                count = count + 1
            Closed -> alive = false
    count

async fn main(console: Console):
    let (first_tx, first_rx) = chan.channel(4).await
    let (second_tx, second_rx) = chan.channel(4).await
    chan.send(first_tx, console).await
    chan.send(second_tx, console).await
    let count = consume(first_rx, second_rx).await
    console.print("${count}")
"#;
    let mut errors = Vec::new();
    for options in [OptSet::none(), OptSet::all()] {
        witchy::opt::set_for_tests(Some(options));
        let error = witchy::resolve_std_only_checked(source).expect_err("capability message descriptors remain unsupported").to_string();
        assert!(error.contains("capability type `Console` cannot have a runtime descriptor"), "{error}");
        errors.push(error);
    }
    assert_eq!(errors[0], errors[1]);
}
