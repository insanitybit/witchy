use super::*;

fn run_owner_list_source(source: &str) -> Result<Vec<String>, String> {
    let checked = witchy::resolve_std_only_checked(source).expect("check owner-list source");
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("admit owner-list source");
    let mut runtime = crate::runtime::Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact, crate::runtime::Capabilities {
        print: true, quiet: true, ..crate::runtime::Capabilities::none()
    }, 128).expect("spawn admitted owner-list source");
    vm.run().map_err(|error| error.root_cause().to_string())?;
    assert_eq!(vm.canonical_heap_leaks().expect("owner-list heap audit"), Some(0));
    Ok(vm.output())
}

const CLOSURES: &str = r#"fn make(own value: Int) -> &Int:
    &value

fn preserve(var values: List(fn() -> Int), replacement: fn() -> Int) -> Int:
    let snapshot = values
    values[0] = replacement
    let original = snapshot[0]
    original()

fn main(console: Console):
    let view = make(11)
    let first = fn() -> Int: *view
    let replacement = fn() -> Int: *view + 1
    var values = [first, first]
    values[0] = replacement
    let current = values[0]
    console.print("${current()}")
    let snapshot = values
    values[0] = first
    let original = snapshot[0]
    console.print("${original()}")
    values[0] = values[0]
    let same = values[0]
    console.print("${same()}")
    console.print("${preserve(values, replacement)}")
    var loop_values = [first, first]
    var index = 0
    while index < 128:
        loop_values[0] = replacement
        index = index + 1
    let final = loop_values[0]
    console.print("${final()}")
"#;

const TASKS: &str = r#"import task

fn main(console: Console):
    let old = task.lazy(fn():
        console.print("old")
        task.ready_unit()
    )
    let new = task.lazy(fn():
        console.print("new")
        task.ready_unit()
    )
    var tasks = [old, old]
    let snapshot = tasks
    tasks[0] = new
    task.run(tasks[0])
    task.run(snapshot[0])
    tasks[0] = tasks[0]
    task.run(tasks[0])
"#;

const MESSAGES: &str = r#"type Message:
    Message(fn() -> Int)

fn make(own value: Int) -> &Int:
    &value

fn read(message: Message) -> Int:
    match message:
        Message(callback) -> callback()

fn main(console: Console):
    let view = make(11)
    let first = Message(fn() -> Int: *view)
    let replacement = Message(fn() -> Int: *view + 1)
    var messages = [first, first]
    messages[0] = replacement
    console.print("${read(messages[0])}")
    let snapshot = messages
    messages[0] = first
    console.print("${read(snapshot[0])}")
    console.print("${read(messages[0])}")
"#;

const CHANNELS: &str = r#"import chan
from chan import Sender

async fn produce(tx: Sender(fn() -> Int), console: Console) -> ():
    let callback = fn() -> Int:
        console.print("payload")
        11
    chan.send(tx, callback).await

async fn main(console: Console):
    let (tx, rx) = chan.channel(2).await
    let handle = chan.spawn(produce(tx, console)).await
    chan.join(handle).await
    let received = chan.recv(rx).await
    match received:
        Some(callback) -> console.print("${callback()}")
        None -> console.print("closed")
"#;

#[test]
fn real_scheduler_owner_list_replacement_consumes_a_list_token() {
    use witchy_wir::canonical_safewir::{Operation, closure_owner_contract::OwnerOperation};
    let checked = witchy::resolve_std_only_checked(CHANNELS).expect("checked scheduler owner lists");
    let candidate = witchy_lower::canonical_pipeline_checks::lowered_candidate(&checked);
    let mut consuming = 0;
    let mut copied = 0;
    for function in candidate.functions() {
        let operations = function.blocks.iter().flat_map(|block| &block.operations).collect::<Vec<_>>();
        for operation in &operations {
            let Operation::ClosureOwner(OwnerOperation::ListSet { list, .. }) = operation else { continue };
            if operations.iter().any(|operation| matches!(operation,
                Operation::ClosureOwner(OwnerOperation::CloneList { destination, .. }) if destination == list)) {
                copied += 1;
            } else {
                consuming += 1;
            }
        }
    }
    assert!(consuming > 0, "real scheduler has no consuming GC ListSet: consuming={consuming}, copied={copied}");
}

#[test]
fn owner_list_replacements_preserve_snapshots_and_release_heap() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) { crate::opt::set_for_tests(None); }
    }
    let _reset = Reset;
    for optimization in [crate::opt::OptSet::none(), crate::opt::OptSet::all()] {
        crate::opt::set_for_tests(Some(optimization));
        assert_eq!(run_owner_list_source(CLOSURES).expect("closure owner lists"), ["12", "12", "11", "11", "12"]);
        assert_eq!(run_owner_list_source(TASKS).expect("Task owner lists"), ["new", "old", "new"]);
        assert_eq!(run_owner_list_source(MESSAGES).expect("message owner lists"), ["12", "12", "11"]);
        assert_eq!(run_owner_list_source(CHANNELS).expect("scheduler owner lists"), ["payload", "11"]);
    }
}
