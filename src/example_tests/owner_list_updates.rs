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
        let previous = loop_values[0]
        let _ = previous()
        loop_values[0] = replacement
        index = index + 1
    let final = loop_values[0]
    console.print("${final()}")
    var captured_values = [first, first]
    let capture = fn() -> Int:
        let original = captured_values[0]
        original()
    captured_values[0] = replacement
    console.print("${capture()}")
    let current_capture_value = captured_values[0]
    console.print("${current_capture_value()}")
    var shadowed = [first, first]
    let inside = if true:
        var shadowed = [replacement, replacement]
        shadowed[0] = first
        let inner = shadowed[0]
        inner()
    else: 0
    console.print("${inside}")
    let restored = shadowed[0]
    console.print("${restored()}")
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

async fn produce(tx: Sender(String), console: Console) -> ():
    chan.send(tx, "first").await
    chan.send(tx, "second").await
    console.print("sent")

async fn main(console: Console):
    let (tx, rx) = chan.channel(2).await
    chan.scope([produce(tx, console)]).await
    let first = chan.recv(rx).await
    let second = chan.recv(rx).await
    match first:
        Some(message) -> console.print(message)
        None -> console.print("closed")
    match second:
        Some(message) -> console.print(message)
        None -> console.print("closed")
"#;

#[test]
fn real_scheduler_owner_list_replacement_consumes_a_list_token() {
    use witchy_wir::canonical_safewir::{Operation, closure_owner_contract::OwnerOperation};
    let checked = witchy::resolve_std_only_checked(CHANNELS).expect("checked scheduler owner lists");
    let candidate = witchy_lower::canonical_pipeline_checks::lowered_candidate(&checked);
    let source = checked.canonical_source().expect("scheduler checked source");
    let names = candidate.source_abi_bindings().entries().iter().filter_map(|binding| {
        let declaration = source.view().closed_callable_declaration(binding.source_declaration_ordinal())?;
        Some((binding.function(), declaration.name.clone()))
    }).collect::<std::collections::BTreeMap<_, _>>();
    let mut consuming = 0;
    let mut copied = 0;
    let mut setters = std::collections::BTreeSet::new();
    for function in candidate.functions() {
        let operations = function.blocks.iter().flat_map(|block| &block.operations).collect::<Vec<_>>();
        let mut direct = 0;
        let mut retained = 0;
        for operation in &operations {
            let Operation::ClosureOwner(OwnerOperation::ListSet { list, .. }) = operation else { continue };
            if operations.iter().any(|operation| matches!(operation,
                Operation::ClosureOwner(OwnerOperation::CloneList { destination, .. }) if destination == list)) {
                copied += 1;
                retained += 1;
            } else {
                consuming += 1;
                direct += 1;
            }
        }
        if direct + retained > 0 {
            setters.insert(function.id);
            println!("owner-list sets {} direct={direct} cloned={retained}", names.get(&function.id).map(String::as_str).unwrap_or("unnamed"));
        }
    }
    for function in candidate.functions() {
        let operations = function.blocks.iter().flat_map(|block| &block.operations).collect::<Vec<_>>();
        let mut inputs = std::collections::BTreeMap::<String, (usize, usize)>::new();
        for operation in &operations {
            let Operation::Call { callee, arguments, .. } = operation else { continue };
            if !setters.contains(callee) { continue; }
            let cloned = arguments.iter().any(|argument| {
                let (witchy_wir::canonical_safewir::CallArgument::Value(value)
                    | witchy_wir::canonical_safewir::CallArgument::Move(value)) = argument;
                operations.iter().any(|operation| matches!(operation,
                    Operation::ClosureOwner(OwnerOperation::CloneList { destination, .. }) if destination == value))
            });
            let counts = inputs.entry(names.get(callee).cloned().unwrap_or_else(|| "unnamed".into())).or_default();
            if cloned { counts.1 += 1; } else { counts.0 += 1; }
        }
        for (callee, (direct, cloned)) in inputs {
            println!("owner-list setter calls {} -> {callee} direct={direct} cloned={cloned}", names.get(&function.id).map(String::as_str).unwrap_or("unnamed"));
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
        assert_eq!(run_owner_list_source(CLOSURES).expect("closure owner lists"), ["12", "12", "11", "11", "12", "11", "12", "11", "11"]);
        assert_eq!(run_owner_list_source(TASKS).expect("Task owner lists"), ["new", "old", "new"]);
        assert_eq!(run_owner_list_source(MESSAGES).expect("message owner lists"), ["12", "12", "11"]);
        assert_eq!(run_owner_list_source(CHANNELS).expect("scheduler owner lists"), ["sent", "first", "second"]);
    }
}
