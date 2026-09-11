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
