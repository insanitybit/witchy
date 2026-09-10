//! RFC-0135/RFC-0165 execution oracle: every admitted program runs through
//! the executable SafeWIR interpreter (the checker's semantics) and through
//! the admitted Wasm; the two must print the same lines, and neither may end
//! with a live reference-counted object. The oracle is what catches an
//! encoder defect the plan mirror faithfully reproduces (the destructor
//! scratch-slot leak was one).

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterConfig, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

#[test]
fn unit_payload_storage_calls_and_effects_three_legs() {
    let source = r#"
type Payload(a):
    Payload(a, Int)

fn unit(console: Console) -> Nil:
    console.print("effect")
    Nil

fn forward(value: Payload(Nil)) -> Payload(Nil):
    value

fn consume(value: Nil, marker: Int) -> Int:
    marker

fn observe(value: Payload(Nil)) -> Int:
    match value:
        Payload(item, marker) -> consume(item, marker)

fn optional(present: Bool) -> Option(Nil):
    if present: Some(Nil) else: None

fn inspect(value: Option(Nil)) -> Int:
    match value:
        Some(item) -> consume(item, 7)
        None -> 3

fn main(console: Console):
    let wrapped = forward(Payload(unit(console), 41))
    console.print("${observe(wrapped)}")
    var values = [Nil, Nil]
    values.push(Nil)
    console.print("${consume(values[2], values.length())}")
    console.print("${inspect(optional(true))} ${inspect(optional(false))}")
"#;
    let expected = ["effect", "41", "3", "7 3"].map(str::to_owned).to_vec();
    let checked = witchy::resolve_std_only_checked(source).expect("unit payload source checks");
    assert_eq!(witchy::interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("source interpreter"), expected);
    assert_eq!(oracle(source, "unit payload storage, calls, and effects"), expected);
}

#[test]
fn own_string_capture_by_value_three_legs() {
    let source = r#"
fn make(own text: String, console: Console) -> fn() -> String:
    let captured = fn(): text
    console.print(text)
    captured

fn main(console: Console):
    let captured = make("held", console)
    console.print(captured())
    console.print(captured())
"#;
    let expected = vec!["held".to_owned(); 3];
    let checked = witchy::resolve_std_only_checked(source).expect("source checks after capture reuse");
    let source_output = witchy::interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("source interpreter");
    assert_eq!(source_output, expected);
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("existing canonical authority accepts by-value capture");
    let entry = artifact.entry().unwrap();
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked).expect("oracle validation");
    let capabilities = entry.capabilities().iter().copied()
        .map(InterpreterValue::entry_capability).collect();
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated, function, capabilities, Default::default(),
    ).expect("oracle execution");
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0);
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
        print: true, quiet: true, ..Default::default()
    }, 256).expect("admitted Wasm");
    vm.run().expect("Wasm execution");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
}

#[test]
fn dynamic_reflection_example_expected_output_three_legs() {
    let source = include_str!("../../examples/dynamic_reflection/src/dynamic_reflection.witchy");
    // This harness links source bytes as module `main`; the CLI links the same
    // file as `dynamic_reflection`. The remaining expectations follow directly
    // from Counter(4), its Named impl, and the public add method called with 3.
    let expected = vec![
        "main.Counter".to_owned(),
        "add".to_owned(),
        "implements Named: true".to_owned(),
        "field value: 4".to_owned(),
        "result: 7".to_owned(),
    ];
    let checked = witchy::resolve_std_only_checked(source).expect("actual Dynamic example checks");
    assert!(checked.canonical_source().unwrap().dynamic_runtime_plan().is_some());
    let source_output = witchy::interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("authenticated source interpreter");
    assert_eq!(source_output, expected);
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("closed Dynamic source admits through the canonical route");
    let entry = artifact.entry().unwrap();
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked).expect("oracle validation");
    let capabilities = entry.capabilities().iter().copied()
        .map(InterpreterValue::entry_capability).collect();
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated, function, capabilities, Default::default(),
    ).expect("Dynamic oracle execution");
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0);
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
        print: true, quiet: true, ..Default::default()
    }, 256).expect("admitted Dynamic Wasm");
    vm.run().expect("Dynamic Wasm execution");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
}

#[test]
fn all_terminal_if_flow_value_three_legs() {
    let source = r#"
fn choose(flag: Bool) -> Int:
    if flag:
        return 4
    else:
        return 9

fn main(console: Console):
    console.print("${choose(true)} ${choose(false)}")
"#;
    let expected = vec!["4 9".to_owned()];
    let checked = witchy::resolve_std_only_checked(source).expect("source checks");
    let source_output = witchy::interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("source interpreter");
    assert_eq!(source_output, expected);
    assert_eq!(oracle(source, "all-terminal if flow value"), expected);
}

#[test]
fn loop_terminators_preserve_owned_returns_three_legs() {
    let source = r#"
fn from_while(xs: List(String), flag: Bool) -> List(String):
    while true:
        if flag:
            return xs
        else:
            return ["fresh"]
    ["unreachable"]

fn from_range(xs: List(String)) -> List(String):
    for n in 0..2:
        return xs
    ["empty"]

fn from_list(xs: List(String)) -> String:
    for item in xs:
        return item
    "empty"

fn main(console: Console):
    let xs = ["held"]
    console.print(from_while(xs, true)[0])
    console.print(from_while(xs, false)[0])
    console.print(from_range(xs)[0])
    console.print(from_list(xs))
    console.print(from_list([]))
    console.print(xs[0])
"#;
    let expected = ["held", "fresh", "held", "held", "empty", "held"]
        .map(str::to_owned).to_vec();
    let checked = witchy::resolve_std_only_checked(source).expect("source checks");
    assert_eq!(witchy::interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("source interpreter"), expected);
    assert_eq!(oracle(source, "loop return preserves owned tokens"), expected);
}

#[test]
fn generators_and_pascal_expected_output_three_legs() {
    let cases: &[(&str, &str, &[&str])] = &[
        ("generators", include_str!("../../examples/generators/src/generators.witchy"), &[
            "fib[0..10): 0, 1, 1, 2, 3, 5, 8, 13, 21, 34",
            "collatz(6): 6, 3, 10, 5, 16, 8, 4, 2, 1",
            "collatz(27) length: 112",
        ]),
        ("pascal", include_str!("../../examples/pascal/src/pascal.witchy"), &[
            "1", "1 1", "1 2 1", "1 3 3 1", "1 4 6 4 1", "1 5 10 10 5 1",
        ]),
    ];
    for (label, source, expected) in cases {
        let expected = expected.iter().map(|line| (*line).to_owned()).collect::<Vec<_>>();
        let checked = witchy::resolve_std_only_checked(source).expect("example checks");
        assert_eq!(witchy::interpreter::run_checked_module(&checked, ".", Vec::new())
            .expect("source interpreter"), expected, "{label}: specified output");
        assert_eq!(oracle(source, label), expected, "{label}: admitted execution");
    }
}

#[test]
fn canonical_console_receiver_precedes_message_three_legs() {
    let source = r#"
fn receiver(console: Console) -> Console:
    console.print("receiver")
    console

fn message(console: Console) -> String:
    console.print("message")
    "payload"

fn main(console: Console):
    receiver(console).print(message(console))
"#;
    let expected = ["receiver", "message", "payload"].map(str::to_owned).to_vec();
    let checked = witchy::resolve_std_only_checked(source).expect("Console expression checks");
    assert_eq!(witchy::interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("source interpreter"), expected);
    assert_eq!(oracle(source, "actual Console receiver and effect order"), expected);
}

fn oracle(source: &str, label: &str) -> Vec<String> {
    let checked = witchy::resolve_std_only_checked(source).unwrap_or_else(|e| panic!("{label}: {e}"));
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered(&format!("{label}: the foundation subset admits this program"));
    let entry = artifact.entry().unwrap_or_else(|| panic!("{label}: a program has an entry"));
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked).unwrap_or_else(|e| panic!("{label}: {e}"));
    let arguments = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
    // The oracle and the runtime both see the working directory and the
    // process environment (the dev grants `witchy run` derives), so an
    // example with `Dir`/`Env` parameters runs on both legs.
    let config = InterpreterConfig {
        compiler_services: Some(witchy::compiler_oracle::call),
        max_steps: 20_000_000_000,
        fixture: witchy_wir::canonical_interpreter::OracleFixture {
            root: Some(std::path::PathBuf::from(".")),
            process_environment: true,
            dir_grants: Some(1),
            ..Default::default()
        },
    };
    let run = interpret_entry_program(&validated, function, arguments, &[], config)
        .unwrap_or_else(|e| panic!("{label}: oracle refused: {e:?}"));

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities {
                print: true,
                print_int: true,
                quiet: true,
                env: true,
                dir_root: Some(std::path::PathBuf::from(".")),
                dir_read: true,
                ..Default::default()
            },
            256,
        )
        .expect("spawn admitted artifact");
    vm.run().unwrap_or_else(|e| panic!("{label}: wasm run failed: {e}"));
    let mut wasm_lines = vm.output();
    // The host prints an `Int`/`Float` result after the entry returns; the
    // oracle reports it as a result value instead.
    let rendered = match run.results.first() {
        Some(InterpreterValue::Int(n)) => Some(n.to_string()),
        Some(InterpreterValue::FloatBits(bits)) => Some(witchy_syntax::fmt::render_float(f64::from_bits(*bits))),
        _ => None,
    };
    if rendered.is_some() {
        let last = wasm_lines.pop();
        assert_eq!(last, rendered, "{label}: entry result differs");
    }
    assert_eq!(run.output, wasm_lines, "{label}: printed lines differ between oracle and wasm");
    assert_eq!(run.leaked_objects, 0, "{label}: the oracle saw objects survive the entry");
    let leaks = vm.canonical_heap_leaks().expect("heap audit").expect("canonical module");
    assert_eq!(leaks, 0, "{label}: the wasm heap kept unreleased objects");
    run.output
}

#[test]
fn nested_if_and_multarm_string_results_reauthenticate_edges() {
    let source = r#"
fn nested(flag: Bool, left: String, right: String) -> String:
    if flag:
        if flag:
            left + "!"
        else:
            right + "?"
    else:
        right + "."

fn decorate(value: String) -> String:
    value + "!"

fn pick(values: List(String), index: Int) -> String:
    match index:
        0 -> decorate(values[0])
        1 -> decorate(values[1])
        _ -> decorate(values[2])

fn choose_values(left: List(Int), right: List(Int), index: Int) -> List(Int):
    match index:
        0 -> left
        _ -> right

fn main(console: Console):
    let values = ["a", "b", "c"]
    let left = [1]
    let right = [2, 3]
    console.print(nested(true, "a", "c"))
    console.print(nested(false, "a", "c"))
    console.print(pick(values, 0))
    console.print(pick(values, 1))
    console.print(pick(values, 2))
    console.print("${choose_values(left, right, 0).length()}")
"#;
    let expected = ["a!", "c.", "a!", "b!", "c!", "1"]
        .into_iter().map(str::to_owned).collect::<Vec<_>>();
    let checked = witchy::resolve_std_only_checked(source).expect("extent result source checks");
    let source_output = witchy::interpreter::run_module_args(
        checked.module().clone(), ".", Vec::new(), Vec::new(),
    ).expect("extent result source interpreter");
    assert_eq!(source_output, expected);
    oracle(source, "nested and multiarm extent results");
}

#[test]
fn integer_list_match_result_is_string_free() {
    let source = r#"
fn choose(left: List(Int), right: List(Int), index: Int) -> List(Int):
    match index:
        0 -> left
        _ -> right

fn main() -> Int:
    choose([4], [7, 8], 1).length()
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("string-free integer match checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("string-free integer match lowers");
    let entry = artifact.entry().expect("string-free integer match entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked)
        .expect("string-free integer match validates");
    let run = interpret_entry_program(
        &validated,
        function,
        Vec::new(),
        &[],
        InterpreterConfig::default(),
    ).expect("string-free integer match oracle");
    assert_eq!(run.results.first(), Some(&InterpreterValue::Int(2)));
    oracle(source, "string-free integer match");
}

#[test]
fn oracle_agrees_on_the_foundation_programs() {
    let programs: &[(&str, &str)] = &[
        ("print", "fn main(console: Console):\n    console.print(\"ok\")\n"),
        ("int result", "fn main() -> Int:\n    40 + 2\n"),
        ("loops", "fn main(console: Console):\n    var i = 0\n    while i < 3:\n        console.print(\"tick\")\n        i = i + 1\n    for j in 0..2:\n        console.print(\"tock\")\n"),
        ("strings", "pub fn shout(name: String) -> String:\n    \"hi \" + name\n\nfn keep(own value: String) -> String:\n    value\n\nfn main(console: Console):\n    console.print(shout(\"x\"))\n    console.print(keep(\"ok\"))\n    console.print(\"${16} ${-7} ${0}\")\n"),
        ("lists", "pub fn dot(a: List(Int), b: List(Int)) -> Int:\n    var sum = 0\n    var i = 0\n    while i < a.length():\n        sum = sum + a[i] * b[i]\n        i = i + 1\n    sum\n\npub fn grow(placed: List(Int), col: Int) -> Int:\n    var next = placed\n    next.push(col)\n    next.length()\n\nfn main(console: Console):\n    let u = [1, 2, 3]\n    console.print(\"${dot(u, [4, 5, 6])} ${grow(u, 9)} ${u.length()}\")\n"),
        ("lists of strings", "pub fn join_all(xs: List(String)) -> String:\n    var out = \"\"\n    for s in xs:\n        out = out + s\n    out\n\nfn main(console: Console):\n    let xs = [\"a\", \"bb\", \"ccc\"]\n    console.print(join_all(xs))\n    console.print(xs[1])\n    var ys = [\"x\"]\n    ys.push(\"yy\")\n    console.print(\"${ys.length()}\")\n"),
        ("list patterns", "pub fn sum(xs: List(Int)) -> Int:\n    match xs:\n        [] -> 0\n        [head, ..tail] -> head + sum(tail)\n\nfn main(console: Console):\n    console.print(\"${sum([3, 7, 11])}\")\n"),
        ("joins", "pub fn pick(xs: List(Int), n: Int) -> List(Int):\n    if xs.length() == n:\n        xs\n    else:\n        var out = xs\n        out.push(n)\n        out\n\npub fn choose(flag: Bool, left: String, right: String) -> String:\n    if flag:\n        left\n    else:\n        right\n\nfn main(console: Console):\n    console.print(\"${pick([1], 1).length()} ${pick([1], 2).length()}\")\n    console.print(choose(true, \"l\", \"r\") + choose(false, \"l\", \"r\"))\n"),
        ("tail calls", "pub fn countdown(n: Int, acc: Int) -> Int:\n    if n == 0:\n        acc\n    else:\n        countdown(n - 1, acc + 1)\n\nfn main(console: Console):\n    console.print(\"${countdown(30000, 0)}\")\n"),
        ("recursion with list threading", "pub fn count(n: Int, placed: List(Int)) -> Int:\n    if placed.length() == n:\n        1\n    else:\n        var total = 0\n        var col = 0\n        while col < n:\n            if col % 2 == 0:\n                var next = placed\n                next.push(col)\n                total = total + count(n, next)\n            col = col + 1\n        total\n\nfn main(console: Console):\n    console.print(\"${count(3, [])}\")\n"),
    ];
    for (label, source) in programs {
        oracle(source, label);
    }
}

#[test]
fn oracle_agrees_on_the_admitted_examples() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut checked_any = false;
    for entry in std::fs::read_dir(root.join("examples")).expect("examples directory") {
        let dir = entry.expect("example entry").path();
        let Some(name) = dir.file_name().and_then(|n| n.to_str()).map(str::to_owned) else { continue };
        let file = dir.join("src").join(format!("{name}.witchy"));
        let Ok(source) = std::fs::read_to_string(&file) else { continue };
        if !source.contains("fn main") {
            continue;
        }
        // Only programs the foundation subset admits are the oracle's
        // business; refused programs are the census's.
        let Ok(checked) = witchy::resolve_std_only_checked(&source) else { continue };
        if !matches!(codegen::compile_checked_module_artifact(&checked), codegen::LoweringOutcome::Lowered(_)) {
            continue;
        }
        oracle(&source, &format!("examples/{name}"));
        checked_any = true;
    }
    assert!(checked_any, "at least one example admits");
}

fn assert_opaque_closure_three_legs(source: &str, expected: &[&str]) {
    let checked = witchy::resolve_std_only_checked(source).expect("opaque closure source checks");
    let expected = expected.iter().map(|line| (*line).to_owned()).collect::<Vec<_>>();
    assert_eq!(witchy::interpreter::run_checked_module(&checked, ".", Vec::new()).expect("source execution"), expected);
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("opaque closure canonical kernel");
    let entry = artifact.entry().expect("entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked).expect("source-bound oracle validation");
    let capabilities = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
    let result = witchy_wir::canonical_interpreter::interpret_program(&validated, function, capabilities, Default::default()).expect("opaque closure oracle");
    assert_eq!(result.output, expected);
    assert_eq!(result.leaked_objects, 0);
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities { print: true, quiet: true, ..Default::default() }, 256).expect("opaque closure admitted Wasm");
    vm.run().expect("opaque closure Wasm execution");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
}

#[test]
fn opaque_closure_plugin_host_expected_three_legs() {
    assert_opaque_closure_three_legs(
        include_str!("../../examples/plugin_host/src/plugin_host.witchy"),
        &["1 -> 12", "5 -> 20", "10 -> 30", "[log] ran the pipeline"],
    );
}

#[test]
fn opaque_closure_mixed_string_console_list_after_original_cleanup_three_legs() {
    assert_opaque_closure_three_legs(r#"
fn prefix(n: Int) -> String:
    "log: " + "${n}"

fn make(own text: String, console: Console) -> fn(String) -> Nil:
    let log = fn(message: String): console.print(text + message)
    console.print(text)
    log

fn main(console: Console):
    let logs = [make(prefix(7), console), make(prefix(8), console)]
    for log in logs:
        log(" A")
    for log in logs:
        log(" B")
"#, &["log: 7", "log: 8", "log: 7 A", "log: 8 A", "log: 7 B", "log: 8 B"]);
}

#[test]
fn opaque_console_lambda_three_legs() {
    assert_opaque_closure_three_legs(
        "fn main(console: Console):\n    let log = fn(message: String): console.print(message)\n    log(\"ok\")\n",
        &["ok"],
    );
}

#[test]
fn opaque_closure_mixed_string_console_after_original_cleanup_three_legs() {
    assert_opaque_closure_three_legs(r#"
fn prefix(n: Int) -> String:
    "log: " + "${n}"

fn make(own text: String, console: Console) -> fn(String) -> Nil:
    let log = fn(message: String): console.print(text + message)
    console.print(text)
    log

fn main(console: Console):
    let log = make(prefix(7), console)
    log(" A")
    log(" B")
"#, &["log: 7", "log: 7 A", "log: 7 B"]);
}
