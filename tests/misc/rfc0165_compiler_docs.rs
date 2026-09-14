//! Documentation is source-as-data, never guest execution or compiler authority.
#[test]
fn compiler_docs_errors_and_argument_snapshots_three_legs() {
    let source = r#"import compiler
fn replace(console: Console, var name: String) -> String:
    console.print("replace")
    name = "Changed"
    "pub fn answer() -> Int:\n    42\n"
fn main(console: Console):
    let code = "pub fn answer() -> Int:\n    42\n"
    console.print(compiler.doc("Demo", code))
    match compiler.try_doc("Demo", code):
        Ok(md) -> console.print(md)
        Err(error) -> console.print(compiler.compiler_error_message(error))
    console.print(compiler.doc("Bad", "pub fn broken("))
    match compiler.try_doc("Bad", "pub fn broken("):
        Ok(md) -> console.print(md)
        Err(error) -> console.print(compiler.compiler_error_message(error))
    let comptime_source = "comptime:\n    abort(\"must not execute\")\n"
    console.print(compiler.doc("CT", comptime_source))
    match compiler.try_doc("CT", comptime_source):
        Ok(md) -> console.print(md)
        Err(error) -> console.print(compiler.compiler_error_message(error))
    var original = "Original"
    console.print(compiler.doc(original, replace(console, original)))
    console.print(original)
    var saved = "Saved"
    match compiler.try_doc(saved, replace(console, saved)):
        Ok(md) -> console.print(md)
        Err(error) -> console.print(compiler.compiler_error_message(error))
    console.print(saved)
"#;
    let checked = witchy::resolve_std_only_checked(source).unwrap();
    let root = crate::temp_dir::TempDir::new("canonical-compiler-docs");
    let expected = witchy::interpreter::run_checked_module(&checked, root.path(), Vec::new()).unwrap();
    assert_eq!(expected.len(), 12);
    assert!(expected[0].starts_with("## `Demo`\n"));
    assert_eq!(expected[0], expected[1]);
    assert!(expected[2].starts_with("<!-- doc error: "));
    assert!(!expected[3].is_empty());
    assert!(expected[4].contains("does not support comptime source strings"));
    assert!(expected[5].starts_with("compiler.try_doc does not support comptime source strings"));
    assert_eq!(expected[6], "replace");
    assert!(expected[7].starts_with("## `Original`\n"));
    assert_eq!(expected[8], "Changed");
    assert_eq!(expected[9], "replace");
    assert!(expected[10].starts_with("## `Saved`\n"));
    assert_eq!(expected[11], "Changed");
    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(&checked, |validated, artifact| {
        use witchy_wir::canonical_interpreter::{InterpreterConfig, InterpreterValue, interpret_entry_program};
        let entry = artifact.entry().unwrap();
        let function = witchy_wir::canonical_safewir::FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let tokens = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
        let run = interpret_entry_program(validated, function, tokens, &[], InterpreterConfig {
            compiler_services: Some(witchy::compiler_oracle::call), ..Default::default()
        }).unwrap();
        assert_eq!(run.output, expected);
        assert_eq!(run.leaked_objects, 0);
    }).expect_lowered("documentation is an authenticated bounded compiler service");
    let mut runtime = witchy::runtime::Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, witchy::runtime::Capabilities {
        print: true, ..witchy::runtime::Capabilities::none()
    }, 256).unwrap();
    vm.run().unwrap();
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
}
