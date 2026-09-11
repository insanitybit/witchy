//! Source-bound named adapters inside nested opaque closure families.

#[test]
fn nested_higher_order_logging_three_legs() {
    use witchy::{codegen, runtime::{Capabilities, Runtime}};
    use witchy_wir::canonical_interpreter::{interpret_program, InterpreterValue};
    use witchy_wir::canonical_safewir::FunctionId;
    let source = r#"
fn logging(console: Console) -> fn(fn(Int) -> Int) -> fn(Int) -> Int:
    fn(next: fn(Int) -> Int):
        fn(n: Int):
            console.print("request ${n}")
            next(n)

fn next(n: Int) -> Int:
    n + 1

fn main(console: Console):
    let layer = logging(console)
    let handler = layer(next)
    console.print("${handler(4)}")
"#;
    let expected = ["request 4", "5"];
    let checked = witchy::resolve_std_only_checked(source).expect("logging source checks");
    assert_eq!(witchy::interpreter::run_checked_module(&checked, ".", Vec::new()).expect("source execution"), expected);
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("nested logging admission and encoding");
    let entry = artifact.entry().expect("entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked).expect("source-bound oracle validation");
    let arguments = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
    let oracle = interpret_program(&validated, function, arguments, Default::default()).expect("canonical oracle");
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0);
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print: true, quiet: true, ..Default::default() }, 256).expect("admitted Wasm");
    vm.run().expect("nested logging Wasm execution");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
}

#[test]
fn admitted_server_workers_reach_entry_with_fresh_opaque_captures() {
    use witchy::{codegen, runtime::{Capabilities, Runtime}};
    let checked = witchy::resolve_std_only_checked(r#"
fn main(console: Console, net: Net):
    let log = fn(): console.print("started")
    let listener = net.listen("127.0.0.1:0")
    listener.serve_pool()
    log()
"#).unwrap();
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("admitted server pool");
    assert!(!artifact.owner_bridges().is_empty());
    assert!(artifact.entry().is_some());
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
        print: true, quiet: true, net_allow: Some(vec!["127.0.0.1:0".into()]), net_listen: true,
        ..Default::default()
    }, 256).unwrap();
    // serve_pool cannot return until every worker has instantiated its own
    // opaque owners and invoked its admitted entry through the rendezvous.
    vm.run().expect("all server workers entered the admitted program");
    assert_eq!(vm.output(), ["started"]);
    assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
}

#[test]
fn server_workers_reject_raw_wasm_even_with_genuine_entry_metadata() {
    use witchy::{codegen, runtime::{Capabilities, Runtime}};
    let checked = witchy::resolve_std_only_checked(r#"
fn main(net: Net):
    let listener = net.listen("127.0.0.1:0")
    listener.serve_pool()
"#).unwrap();
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("entry metadata fixture");
    assert!(artifact.owner_bridges().is_empty());
    assert_eq!(witchy_wir::canonical_entry_from_wasm(artifact.wasm()).unwrap().as_ref(), artifact.entry());
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn(artifact.wasm(), Capabilities {
        net_allow: Some(vec!["127.0.0.1:0".into()]), net_listen: true, ..Default::default()
    }, 256).unwrap();
    let error = vm.run().expect_err("entry bytes cannot replace sealed admitted worker metadata");
    assert!(format!("{error:#}").contains("server worker requires an admitted program with canonical entry metadata"));
}

#[test]
fn admitted_server_workers_preserve_launch_arguments_before_rendezvous() {
    use witchy::{codegen, runtime::{Capabilities, Runtime}};
    let checked = witchy::resolve_std_only_checked(r#"
fn main(console: Console, args: List(String), net: Net):
    if args.length() != 3:
        fail("launch argument count changed")
    if args[0] != "worker input" || args[1] != "" || args[2] != "last value":
        fail("launch argument values changed")
    let listener = net.listen("127.0.0.1:0")
    listener.serve_pool()
    console.print(args[0] + ":" + args[2])
"#).unwrap();
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("argument-bearing pool entry");
    assert_eq!(artifact.entry().unwrap().argument_positions(), &[1]);
    let caps = Capabilities {
        print: true, quiet: true, net_allow: Some(vec!["127.0.0.1:0".into()]), net_listen: true,
        args: vec!["worker input".into(), "".into(), "last value".into()],
        ..Default::default()
    };
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, caps.clone(), 256).unwrap();
    // This is the normal entry API. A worker losing any argument traps before
    // rendezvous, so serve_pool must return its startup failure to this run.
    vm.run().expect("primary and pool workers preserve launch arguments");
    assert_eq!(vm.output(), ["worker input:last value"]);
    assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);

    // Pin that the pre-rendezvous guard is executable, not a discarded check.
    let mut invalid = runtime.spawn_admitted(&artifact, Capabilities {
        args: Vec::new(), ..caps
    }, 256).unwrap();
    assert!(invalid.run().is_err());
    assert!(invalid.output().is_empty());
}

#[path = "rfc0165_vm_service.rs"]
mod vm_service;
