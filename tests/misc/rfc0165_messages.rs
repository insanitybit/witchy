//! Internal toolchain erasure fixtures use the ordinary source-bearing loader
//! and admission APIs. They do not make the private bridge callable by users.
use std::collections::HashSet;
use witchy::runtime::{Capabilities, Runtime};
use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};
use witchy_syntax::origin::OriginTable;
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

fn checked(task: &str, main: &str) -> witchy_types::pipeline::CheckedModule {
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/stdlib", "0.1.0").unwrap();
    let application = PackageCoordinate::new(PackageSource::Workspace, "test/messages", "0.1.0").unwrap();
    let mut owners = vec![("main".into(), ModuleLoadIdentity::new(application, ["main"]).unwrap())];
    owners.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| {
        ((*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", name]).unwrap())
    }));
    witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        vec![("main".into(), main.as_bytes().to_vec()), ("task".into(), task.as_bytes().to_vec())],
        "main", |_, _, _| Ok(OriginTable::default()), &HashSet::from(["main".into()]),
        AuthenticatedModuleOwners::from_loader_assignments(owners).unwrap(),
    ).expect("approved primitive message source fixture")
}

const BRIDGE: &str = r#"
fn pack_int(value: Int) -> __Msg:
    __erase(value)
fn pack_text(value: String) -> __Msg:
    __erase(value)
fn forward(value: __Msg) -> __Msg:
    value
fn unpack_int(value: __Msg) -> Int:
    __unerase(value)
fn unpack_text(value: __Msg) -> String:
    __unerase(value)
pub fn exercise() -> String:
    var text = "held"
    let message = pack_text(text)
    text = "changed"
    let ring = [pack_int(0), message]
    let delivered = forward(ring[1])
    "${unpack_int(ring[0])} ${unpack_text(delivered)} ${text}"
pub fn mismatch() -> Int:
    unpack_int(pack_text("wrong"))
"#;

#[test]
fn canonical_message_calls_storage_and_owned_results_three_legs() {
    let checked = checked(BRIDGE, "import task\nfn main(console: Console):\n    console.print(task.exercise())\n");
    let expected = vec!["0 held changed".to_owned()];
    assert_eq!(witchy::interpreter::run_checked_module(&checked, ".", vec![]).unwrap(), expected);
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("message source admits");
    let entry = artifact.entry().unwrap();
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let arguments = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked).unwrap();
    let oracle = interpret_program(&validated, function, arguments, Default::default()).unwrap();
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0);
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities { print: true, quiet: true, ..Default::default() }, 256).unwrap();
    vm.run().unwrap();
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
}

#[test]
fn canonical_message_wrong_payload_tag_traps_before_recovery() {
    let checked = checked(BRIDGE, "import task\nfn main() -> Int:\n    task.mismatch()\n");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("wrong-tag source remains a checked trapping program");
    let entry = artifact.entry().unwrap();
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked).unwrap();
    assert!(interpret_program(&validated, function, vec![], Default::default()).is_err());
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities::default(), 256).unwrap();
    assert!(vm.run().is_err(), "a String message must not be read as Int");
}

#[test]
fn canonical_opaque_variant_list_replacement_preserves_old_closure_three_legs() {
    let source = r#"
type Action:
    Empty
    Emit(fn() -> ())

fn make(console: Console, text: String) -> Action:
    Emit(fn(): console.print(text))

fn emit(console: Console, action: Action):
    match action:
        Empty -> console.print("empty")
        Emit(run) -> run()

fn main(console: Console):
    var actions = [make(console, "one"), make(console, "two")]
    let old = actions
    actions.set_at(0, Empty)
    emit(console, old[0])
    emit(console, actions[0])
    emit(console, actions[1])
"#;
    let checked = witchy::resolve_std_only_checked(source).unwrap();
    let expected = vec!["one".to_owned(), "empty".to_owned(), "two".to_owned()];
    assert_eq!(
        witchy::interpreter::run_checked_module(&checked, ".", vec![]).unwrap(),
        expected,
    );
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("opaque variants and list replacement admit");
    let entry = artifact.entry().unwrap();
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let arguments = entry.capabilities().iter().copied()
        .map(InterpreterValue::entry_capability).collect();
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked).unwrap();
    let oracle = interpret_program(&validated, function, arguments, Default::default()).unwrap();
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0);
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities { print: true, quiet: true, ..Default::default() },
        256,
    ).unwrap();
    vm.run().unwrap();
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
}

#[test]
fn canonical_channel_scheduler_three_legs() {
    let source = include_str!("../../examples/channels/src/channels.witchy");
    let checked = witchy::resolve_std_only_checked(source).unwrap();
    let expected = ["got 1", "got 2", "got 3", "got 4", "channel drained"]
        .map(str::to_owned).to_vec();
    assert_eq!(
        witchy::interpreter::run_checked_module(&checked, ".", vec![]).unwrap(),
        expected,
    );
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("the checked channel scheduler admits");
    let entry = artifact.entry().unwrap();
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let arguments = entry.capabilities().iter().copied()
        .map(InterpreterValue::entry_capability).collect();
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked).unwrap();
    let oracle = interpret_program(&validated, function, arguments, Default::default()).unwrap();
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0);
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities { print: true, quiet: true, ..Default::default() },
        256,
    ).unwrap();
    vm.run().unwrap();
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
}
