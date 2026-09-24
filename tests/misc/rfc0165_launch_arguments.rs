use witchy::{codegen, runtime::{Capabilities, Runtime}};
use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterConfig, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

fn compare(source: &str, args: &[&str], expected: &[&str]) {
    compare_with_fixture(source, args, expected, None);
}

fn compare_with_fixture(source: &str, args: &[&str], expected: &[&str], root: Option<&std::path::Path>) {
    let checked = witchy::resolve_std_only_checked(source).expect("checked launch fixture");
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("launch args admit");
    let entry = artifact.entry().expect("main entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked).expect("oracle validation");
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let source_output = witchy::interpreter::run_module_args(
        checked.module().clone(), root.unwrap_or(std::path::Path::new(".")).to_str().unwrap(),
        Vec::new(), args.clone(),
    ).expect("source interpreter launch");
    let tokens = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
    let mut oracle = interpret_entry_program(&validated, function, tokens, &args, InterpreterConfig {
        max_steps: 20_000_000,
        fixture: witchy_wir::canonical_interpreter::OracleFixture {
            root: root.map(std::path::Path::to_owned),
            dir_grants: Some(1),
            ..Default::default()
        },
        ..Default::default()
    })
        .expect("semantic launch");
    // The source runner reports console effects only. The canonical host
    // additionally prints an Int main result after return; check that result
    // below rather than pretending it was a source console effect.
    assert_eq!(source_output, oracle.output);
    if let Some(InterpreterValue::Int(result)) = oracle.results.first() {
        oracle.output.push(result.to_string());
    }
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0);
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
        print: true, print_int: true, quiet: true, args,
        dir_root: root.map(std::path::Path::to_owned), dir_read: root.is_some(), env: root.is_some(),
        ..Default::default()
    }, 256).expect("spawn");
    vm.run().expect("execute");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().expect("audit"), Some(0));
}

#[test]
fn launch_arguments_survive_proper_tail_transfer() {
    compare("pub fn count(own xs: List(String), n: Int) -> Int:\n    if n == 0:\n        xs.length()\n    else:\n        count(xs, n - 1)\n\nfn main(own args: List(String)) -> Int:\n    count(args, 300000)\n",
        &["alive"], &["1"]);
}

#[test]
fn launch_arguments_do_not_admit_other_reference_parameters() {
    for source in ["fn main(args: List(Int)):\n    ()\n", "fn main(args: String):\n    ()\n"] {
        let error = match witchy::resolve_std_only_checked(source) {
            Ok(_) => panic!("unsupported entry input passed the source boundary"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("main") && error.contains("List(String)"), "{error}");
    }
}
