//! Tail forwarding must preserve lexical writeback identity, not spelling.
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterConfig, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

#[test]
fn canonical_var_tail_preserves_shadowed_binding_and_scope_exit() {
    for (body, expected) in [
        ("    if true:\n        var n = 10\n        operation(n)\n    else:\n        0\n", "1 120"),
        ("    if true:\n        var n = 10\n        bump(n)\n    else:\n        0\n", "1 120"),
        ("    let ignored = if true:\n        var n = 10\n        operation(n)\n    else:\n        0\n    operation(n)\n", "3 30"),
    ] {
        let source = format!("fn bump(var n: Int) -> Int:\n    n = n + 2\n    n * 10\n\nfn apply(operation: fn(var Int) -> Int, var n: Int) -> Int:\n{body}\nfn main(console: Console):\n    var n = 1\n    let answer = apply(bump, n)\n    console.print(\"${{n}} ${{answer}}\")\n");
        let checked = witchy::resolve_std_only_checked(&source).unwrap();
        let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("lexically scoped var writeback admits");
        let validated = witchy::codegen::validate_checked_module_for_oracle(&checked).unwrap();
        let entry = artifact.entry().unwrap();
        let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let capabilities = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
        let oracle = interpret_entry_program(&validated, function, capabilities, &[], InterpreterConfig::default()).unwrap();
        assert_eq!(oracle.output, [expected]);
        assert_eq!(oracle.leaked_objects, 0);
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
            print: true, quiet: true, ..Default::default()
        }, 256).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), [expected]);
        assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
    }
}
