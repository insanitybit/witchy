//! RFC-0135/RFC-0165 execution oracle: every admitted program runs through
//! the executable SafeWIR interpreter (the checker's semantics) and through
//! the admitted Wasm; both must match explicit fixture expectations,
//! and neither may end with a live reference-counted object. The oracle catches an
//! encoder defect the plan mirror faithfully reproduces (the destructor
//! scratch-slot leak was one).

use std::time::{Duration, Instant};

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterConfig, InterpreterValue};
use witchy_wir::canonical_safewir::{FunctionId, ValidatedSafeWir};

#[path = "rfc0165_exec.rs"]
mod exec;

#[test]
fn admitted_observer_preserves_exact_artifact_and_execution() {
    let source = r#"
fn make(console: Console, text: String) -> fn() -> ():
    fn(): console.print(text)

fn main(console: Console):
    let log = make(console, "held" + "!")
    log()
    log()
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("observer source checks");
    let ordinary = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("ordinary admission");
    assert!(!ordinary.owner_bridges().is_empty(), "exercise sealed owner metadata");
    let mut calls = 0;
    let (observed, output) = codegen::compile_checked_module_artifact_with_observer(
        &checked, |validated, artifact| {
            calls += 1;
            assert!(artifact == &ordinary, "complete artifact equality inside observer");
            oracle_validated(validated, artifact, "observed owner capture")
        },
    ).expect_lowered("observed admission");
    assert_eq!(calls, 1);
    assert!(observed == ordinary, "bytes and all sealed metadata remain identical");
    assert_eq!(output, ["held!", "held!"]);
}

#[test]
fn admitted_observer_is_not_called_on_refusal() {
    // An open library parameter has no authenticated closed Dynamic payload
    // catalog. Unlike a newly supported entry capability, this is an explicit
    // missing-authority refusal rather than a coverage-dependent placeholder.
    let unsupported = witchy::resolve_std_only_checked(
        "import dynamic\nfn forward(value: dynamic.Dynamic) -> Result(dynamic.Dynamic, dynamic.DynamicError):\n    dynamic.field(value, \"value\")\n",
    ).expect("open Dynamic library source checks without inventing payload authority");
    let ast = witchy_syntax::parser::parse_module("fn main() -> Int:\n    1\n")
        .expect("AST-only source parses");
    let ast_only = witchy_types::pipeline::link_checked(
        vec![("main".to_owned(), ast)], "main", |_, _, _| Ok(Default::default()),
    ).expect("AST-only module passes real source checking");
    assert!(ast_only.canonical_source().is_none(), "AST checking cannot mint retained source authority");
    for (checked, unsupported) in [(unsupported, true), (ast_only, false)] {
        let mut calls = 0;
        let outcome = codegen::compile_checked_module_artifact_with_observer(
            &checked, |_, _| { calls += 1; },
        );
        assert_eq!(calls, 0, "no observer before successful admission");
        match (outcome, unsupported) {
            (codegen::LoweringOutcome::Unsupported(_), true)
            | (codegen::LoweringOutcome::Rejected(_), false) => {}
            _ => panic!("refusal classification must not change"),
        }
    }
}

#[test]
fn admitted_observer_examples_three_legs() {
    for (label, source) in [
        ("hello", include_str!("../../examples/hello/src/hello.witchy")),
        ("wrap", include_str!("../../examples/wrap/src/wrap.witchy")),
        ("jq", include_str!("../../examples/jq/src/jq.witchy")),
    ] {
        oracle(source, label);
    }
}

#[test]
fn result_coalescing_is_lazy_and_releases_both_variant_payloads() {
    let source = r#"
fn choose(success: Bool) -> Result(String, String):
    if success:
        Ok("value" + "!")
    else:
        Err("unused" + " error")

fn default_value(console: Console) -> String:
    console.print("default")
    "fallback" + "!"

fn forbidden_default(should_fail: Bool) -> String:
    if should_fail:
        fail("the success edge evaluated its default")
    "unreachable"

fn main(console: Console):
    console.print(choose(true) ?? forbidden_default(true))
    console.print(choose(false) ?? default_value(console))
"#;
    let expected = ["value!", "default", "fallback!"].map(str::to_owned).to_vec();
    assert_eq!(oracle(source, "lazy Result coalescing and payload cleanup"), expected);
}

#[test]
fn tail_fail_keeps_trap_semantics_three_legs() {
    let template = r#"
fn check_header(valid: Bool):
    if !valid:
        fail("rejected header")

fn main(console: Console):
    console.print("before")
    check_header(FLAG)
    console.print("after")
"#;
    for (flag, traps) in [("true", false), ("false", true)] {
        let source = template.replace("FLAG", flag);
        let checked = witchy::resolve_std_only_checked(&source).expect("tail fail source checks");
        let expected = ["before", "after"].map(str::to_owned).to_vec();
        let artifact = codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("a tail trap is a complete block terminator");
        let entry = artifact.entry().unwrap();
        let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let validated = codegen::validate_checked_module_for_oracle(&checked).expect("oracle validation");
        let capabilities = entry.capabilities().iter().copied()
            .map(InterpreterValue::entry_capability).collect();
        let oracle_run = witchy_wir::canonical_interpreter::interpret_program(
            &validated, function, capabilities, Default::default(),
        );
        if traps {
            assert_eq!(oracle_run.unwrap_err(),
                witchy_wir::canonical_interpreter::InterpretError::DiagnosticAbort {
                    message: "runtime error: `main.check_header`, line 4: rejected header".into(),
                    output: vec!["before".into()],
                });
        } else {
            let run = oracle_run.expect("oracle execution");
            assert_eq!(run.output, expected);
            assert_eq!(run.leaked_objects, 0);
        }
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
            print: true, quiet: true, ..Default::default()
        }, 256).expect("admitted Wasm");
        let result = vm.run();
        if traps {
            let error = result.expect_err("compiled fail must abort");
            assert_eq!(error.root_cause().to_string(), "runtime error: `main.check_header`, line 4: rejected header");
            assert_eq!(vm.output(), ["before"]);
        } else {
            result.expect("Wasm execution");
            assert_eq!(vm.output(), expected);
            assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
        }
    }
}

#[test]
fn indirect_unit_parameters_and_results_three_legs() {
    let source = r#"
fn number(value: ()) -> Int:
    7

fn identity(value: ()) -> ():
    value

fn tail_apply(f: fn(()) -> Int) -> Int:
    f(())

fn call_apply(f: fn(()) -> Int) -> Int:
    let result = f(())
    result + 1

fn tail_unit(f: fn(()) -> ()) -> ():
    f(())

fn main(console: Console):
    console.print("${tail_apply(number)} ${call_apply(number)}")
    tail_unit(identity)
    console.print("unit")
"#;
    let expected = ["7 8", "unit"].map(str::to_owned).to_vec();
    assert_eq!(oracle(source, "Unit arguments and results through indirect calls"), expected);
}

#[test]
fn unit_capture_with_capability_three_legs() {
    let source = r#"
fn consume(value: ()) -> Int:
    7

fn make(console: Console, value: ()) -> fn() -> Int:
    fn():
        console.print("tick")
        consume(value)

fn main(console: Console):
    let log = make(console, ())
    console.print("${log()}")
"#;
    let expected = ["tick", "7"].map(str::to_owned).to_vec();
    assert_eq!(oracle(source, "Unit and capability closure capture"), expected);
}

#[test]
fn unit_payload_storage_calls_and_effects_three_legs() {
    let source = r#"
type Payload(a):
    Payload(a, Int)

fn unit(console: Console) -> ():
    console.print("effect")
    ()

fn forward(value: Payload(())) -> Payload(()):
    value

fn consume(value: (), marker: Int) -> Int:
    marker

fn observe(value: Payload(())) -> Int:
    match value:
        Payload(item, marker) -> consume(item, marker)

fn optional(present: Bool) -> Option(()):
    if present: Some(()) else: None

fn inspect(value: Option(())) -> Int:
    match value:
        Some(item) -> consume(item, 7)
        None -> 3

fn main(console: Console):
    let wrapped = forward(Payload(unit(console), 41))
    console.print("${observe(wrapped)}")
    var values = [(), ()]
    values.push(())
    console.print("${consume(values[2], values.length())}")
    console.print("${inspect(optional(true))} ${inspect(optional(false))}")
"#;
    let expected = ["effect", "41", "3", "7 3"].map(str::to_owned).to_vec();
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
fn pattern_callable_capture_three_legs() {
    let source = r#"
type Handler:
    Handler(fn(Int) -> Int)

fn make(handler: Handler) -> fn(Int) -> Int:
    match handler:
        Handler(cont) -> fn(value: Int): cont(value)

fn increment(value: Int) -> Int:
    value + 1

fn main(console: Console):
    let callback = make(Handler(increment))
    console.print("${callback(41)}")
    console.print("${callback(9)}")
"#;
    let expected = vec!["42".to_owned(), "10".to_owned()];
    assert_eq!(oracle(source, "pattern callable capture"), expected);
}

#[test]
fn list_indexed_update_preserves_aliases_and_nullable_payloads_three_legs() {
    let source = r#"
fn text(value: Option(String)) -> String:
    match value:
        Some(value) -> value
        None -> "none"

fn main(console: Console):
    var numbers = [1, 2, 3]
    let original = numbers
    numbers[0] = 9
    numbers.set_at(2, 8)
    console.print("${numbers[0]} ${numbers[1]} ${numbers[2]} ${numbers.length()}")
    console.print("${original[0]} ${original[1]} ${original[2]}")
    var values: List(Option(String)) = [Some("held"), None, Some("tail")]
    let previous = values
    values[0] = None
    values[1] = Some("new")
    console.print("${text(values[0])} ${text(values[1])} ${text(values[2])} ${values.length()}")
    console.print("${text(previous[0])} ${text(previous[1])} ${text(previous[2])}")
"#;
    let expected = ["9 2 8 3", "1 2 3", "none new tail 3", "held none tail"]
        .map(str::to_owned).to_vec();
    assert_eq!(oracle(source, "indexed replacement preserves owned values"), expected);
}

#[test]
fn list_indexed_update_rejects_negative_length_and_empty_indices_three_legs() {
    for (initial, index) in [("[1]", -1), ("[1]", 1), ("[]", 0)] {
        let source = format!("fn main(console: Console):\n    var values: List(Int) = {initial}\n    values[{index}] = 7\n    console.print(\"unreachable\")\n");
        let checked = witchy::resolve_std_only_checked(&source).expect("index is a runtime obligation");
        let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("checked failing index admits");
        let entry = artifact.entry().unwrap();
        let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let validated = codegen::validate_checked_module_for_oracle(&checked).expect("independent validation");
        let capabilities = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
        assert!(witchy_wir::canonical_interpreter::interpret_program(&validated, function, capabilities, Default::default()).is_err());
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact, Capabilities { print: true, quiet: true, ..Default::default() }, 256).unwrap();
        assert!(vm.run().is_err());
        assert!(vm.output().is_empty());
    }
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
fn dynamic_library_forwarding_does_not_mint_closed_payload_authority() {
    let source = "import dynamic\nfn forward(value: dynamic.Dynamic) -> Result(dynamic.Dynamic, dynamic.DynamicError):\n    dynamic.field(value, \"value\")\n";
    let checked = witchy::resolve_std_only_checked(source).expect("open library signature type-checks");
    assert!(checked.canonical_source().unwrap().typed_module().module().items.iter().all(|item| {
        !matches!(item, witchy::ast::Item::Type(definition) if definition.name == "$canonical.DynamicPayload")
    }));
    codegen::compile_checked_module_artifact(&checked)
        .expect_unsupported("an open existential is not closed payload authority");
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
    assert_eq!(oracle(source, "actual Console receiver and effect order"), expected);
}

fn oracle(source: &str, label: &str) -> Vec<String> {
    let checked = witchy::resolve_std_only_checked(source).unwrap_or_else(|e| panic!("{label}: {e}"));
    let (_, output) = codegen::compile_checked_module_artifact_with_observer(
        &checked, |validated, artifact| oracle_validated(validated, artifact, label),
    )
        .expect_lowered(&format!("{label}: the foundation subset admits this program"));
    output
}

fn oracle_validated(
    validated: &ValidatedSafeWir,
    artifact: &witchy_wir::AdmittedCanonicalArtifact,
    label: &str,
) -> Vec<String> {
    oracle_validated_profiled(validated, artifact, label, None)
}

#[derive(Default)]
struct OraclePhaseTiming {
    canonical_interpret: Duration,
    runtime_setup: Duration,
    spawn_admitted: Duration,
    wasm_run: Duration,
    heap_audit: Duration,
}

fn oracle_validated_profiled(
    validated: &ValidatedSafeWir,
    artifact: &witchy_wir::AdmittedCanonicalArtifact,
    label: &str,
    mut timing: Option<&mut OraclePhaseTiming>,
) -> Vec<String> {
    // Each nextest shard has its own process; install compiler-service natives
    // before running the canonical interpreter.
    witchy_interp::compiler_natives::install();
    let entry = artifact.entry().unwrap_or_else(|| panic!("{label}: a program has an entry"));
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let arguments = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
    // The oracle and the runtime both see the working directory and the
    // process environment (the dev grants `witchy run` derives), so an
    // example with `Dir`/`Env` parameters runs on both legs.
    let config = InterpreterConfig {
        console_trace: None,
        compiler_services: Some(witchy::compiler_oracle::call),
        max_steps: 20_000_000_000,
        fixture: witchy_wir::canonical_interpreter::OracleFixture {
            root: Some(std::path::PathBuf::from(".")),
            process_environment: true,
            dir_grants: Some(1),
            ..Default::default()
        },
    };
    let phase_started = timing.as_ref().map(|_| Instant::now());
    let run = interpret_entry_program(validated, function, arguments, &[], config)
        .unwrap_or_else(|e| panic!("{label}: oracle refused: {e:?}"));
    if let (Some(timing), Some(started)) = (timing.as_deref_mut(), phase_started) {
        timing.canonical_interpret = started.elapsed();
    }

    let phase_started = timing.as_ref().map(|_| Instant::now());
    // This corpus checks observable equivalence, admission, and heap ownership;
    // it does not assess Cranelift's Speed-tier code quality. Keep the same
    // sandbox and ABI while avoiding expensive optimization of large examples.
    let mut runtime = Runtime::batch_quick().expect("runtime");
    if let (Some(timing), Some(started)) = (timing.as_deref_mut(), phase_started) {
        timing.runtime_setup = started.elapsed();
    }
    let phase_started = timing.as_ref().map(|_| Instant::now());
    let mut vm = runtime
        .spawn_admitted(
            artifact,
            Capabilities {
                print: true,
                print_int: true,
                quiet: true,
                env: true,
                dir_root: Some(std::path::PathBuf::from(".")),
                dir_read: true,
                // Public but uncalled functions can retain Net imports even
                // when main has no Net parameter. Link them with an explicit
                // deny-all fixture: no destination or listener is authorized.
                net_allow: Some(Vec::new()),
                net_connect: true,
                net_listen: true,
                ..Default::default()
            },
            256,
        )
        .expect("spawn admitted artifact");
    if let (Some(timing), Some(started)) = (timing.as_deref_mut(), phase_started) {
        timing.spawn_admitted = started.elapsed();
    }
    let phase_started = timing.as_ref().map(|_| Instant::now());
    vm.run().unwrap_or_else(|e| panic!("{label}: wasm run failed: {e:#}"));
    if let (Some(timing), Some(started)) = (timing.as_deref_mut(), phase_started) {
        timing.wasm_run = started.elapsed();
    }
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
    let phase_started = timing.as_ref().map(|_| Instant::now());
    let leaks = vm.canonical_heap_leaks().expect("heap audit").expect("canonical module");
    if let (Some(timing), Some(started)) = (timing.as_deref_mut(), phase_started) {
        timing.heap_audit = started.elapsed();
    }
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
    assert_eq!(oracle(source, "nested and multiarm extent results"), expected);
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
        ("float result", "fn main() -> Float:\n    1.5\n"),
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

macro_rules! oracle_example_shards {
    ($($name:ident => $index:literal),+ $(,)?) => {
        const ORACLE_SHARD_IDS: &[usize] = &[$($index),+];
        $(
            #[test]
            fn $name() {
                assert!(ORACLE_SHARD_IDS.iter().copied().eq(0..ORACLE_SHARD_IDS.len()),
                    "every oracle shard must be registered exactly once");
                oracle_examples_shard($index, ORACLE_SHARD_IDS.len());
            }
        )+
    };
}

oracle_example_shards! {
    oracle_agrees_on_the_admitted_examples => 0,
    oracle_agrees_on_the_admitted_examples_shard_1 => 1,
    oracle_agrees_on_the_admitted_examples_shard_2 => 2,
    oracle_agrees_on_the_admitted_examples_shard_3 => 3,
    oracle_agrees_on_the_admitted_examples_shard_4 => 4,
    oracle_agrees_on_the_admitted_examples_shard_5 => 5,
    oracle_agrees_on_the_admitted_examples_shard_6 => 6,
    oracle_agrees_on_the_admitted_examples_shard_7 => 7,
    oracle_agrees_on_the_admitted_examples_shard_8 => 8,
    oracle_agrees_on_the_admitted_examples_shard_9 => 9,
    oracle_agrees_on_the_admitted_examples_shard_10 => 10,
    oracle_agrees_on_the_admitted_examples_shard_11 => 11,
    oracle_agrees_on_the_admitted_examples_shard_12 => 12,
    oracle_agrees_on_the_admitted_examples_shard_13 => 13,
    oracle_agrees_on_the_admitted_examples_shard_14 => 14,
    oracle_agrees_on_the_admitted_examples_shard_15 => 15,
}

fn oracle_examples_shard(shard: usize, shards: usize) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let requested = std::env::var("WITCHY_ORACLE_EXAMPLE").ok();
    let emit_timing = std::env::var_os("WITCHY_ORACLE_TIMING").is_some();
    let mut checked_any = false;
    for entry in std::fs::read_dir(root.join("examples")).expect("examples directory") {
        let dir = entry.expect("example entry").path();
        let Some(name) = dir.file_name().and_then(|n| n.to_str()).map(str::to_owned) else { continue };
        if requested.as_deref().is_some_and(|requested| requested != name) {
            continue;
        }
        // Stable assignment is independent of read_dir order and process hash
        // seeds. The registered shards partition the original corpus exactly;
        // no eligibility rule or execution assertion is relaxed. The task
        // examples each cost one to three minutes of compile time, so two of
        // them in one shard bound the whole suite's wall time; the byte-sum
        // buckets put `for_await` with `channels` and `worker_pool` with
        // `async_tasks`, and those two are pinned to otherwise light shards.
        const PINNED: &[(&str, usize)] = &[("for_await", 1), ("worker_pool", 2)];
        let bucket = PINNED
            .iter()
            .find(|(pinned, _)| *pinned == name)
            .map(|(_, shard)| *shard % shards)
            .unwrap_or_else(|| name.bytes().fold(0usize, |sum, byte| sum.wrapping_add(usize::from(byte))) % shards);
        if bucket != shard {
            continue;
        }
        let file = dir.join("src").join(format!("{name}.witchy"));
        let Ok(source) = std::fs::read_to_string(&file) else { continue };
        if !source.contains("fn main") {
            continue;
        }
        eprintln!("oracle sweep: {}", file.display());
        // Only programs the foundation subset admits are the oracle's
        // business; refused programs are the census's.
        let total_started = emit_timing.then(Instant::now);
        let phase_started = emit_timing.then(Instant::now);
        let Ok(checked) = witchy::resolve_std_only_checked(&source) else { continue };
        let check = phase_started.map_or(Duration::ZERO, |started| started.elapsed());
        let phase_started = emit_timing.then(Instant::now);
        let mut observer_elapsed = Duration::ZERO;
        let mut oracle_phases = None;
        let outcome = codegen::compile_checked_module_artifact_with_observer(
            &checked, |validated, artifact| {
                // The semantic oracle deliberately has no network-I/O world.
                // Preserve the sweep's policy after full production admission.
                if artifact.entry().is_some_and(|entry| entry.capabilities().contains(
                    &witchy_wir::CanonicalEntryCapability::Net,
                )) {
                    return false;
                }
                let observer_started = emit_timing.then(Instant::now);
                let mut phases = OraclePhaseTiming::default();
                oracle_validated_profiled(
                    validated,
                    artifact,
                    &format!("examples/{name}"),
                    emit_timing.then_some(&mut phases),
                );
                observer_elapsed = observer_started
                    .map_or(Duration::ZERO, |started| started.elapsed());
                oracle_phases = emit_timing.then_some(phases);
                true
            });
        let admission = phase_started.map_or(Duration::ZERO, |started| started.elapsed());
        let codegen::LoweringOutcome::Lowered((_, ran)) = outcome
        else { continue };
        if let Some(phases) = oracle_phases {
            // The production observer runs only after lowering and admission;
            // its public boundary does not split those two phases further.
            let production_lowering_admission = admission.saturating_sub(observer_elapsed);
            let total = total_started.map_or(Duration::ZERO, |started| started.elapsed());
            eprintln!(
                "WITCHY_ORACLE_TIMING\texample={name}\tcheck_ms={:.3}\tproduction_lowering_admission_ms={:.3}\tcanonical_interpret_ms={:.3}\truntime_setup_ms={:.3}\tspawn_admitted_ms={:.3}\twasm_run_ms={:.3}\theap_audit_ms={:.3}\ttotal_ms={:.3}",
                check.as_secs_f64() * 1_000.0,
                production_lowering_admission.as_secs_f64() * 1_000.0,
                phases.canonical_interpret.as_secs_f64() * 1_000.0,
                phases.runtime_setup.as_secs_f64() * 1_000.0,
                phases.spawn_admitted.as_secs_f64() * 1_000.0,
                phases.wasm_run.as_secs_f64() * 1_000.0,
                phases.heap_audit.as_secs_f64() * 1_000.0,
                total.as_secs_f64() * 1_000.0,
            );
        }
        checked_any |= ran;
    }
    assert!(checked_any, "at least one requested example admits in shard {shard}");
}

fn assert_opaque_closure_three_legs(source: &str, expected: &[&str]) {
    assert_opaque_closure_three_legs_with_caps(source, expected,
        Capabilities { print: true, quiet: true, ..Default::default() });
}

fn assert_opaque_closure_three_legs_with_caps(source: &str, expected: &[&str], caps: Capabilities) {
    let checked = witchy::resolve_std_only_checked(source).expect("opaque closure source checks");
    let expected = expected.iter().map(|line| (*line).to_owned()).collect::<Vec<_>>();
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("opaque closure canonical kernel");
    let entry = artifact.entry().expect("entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked).expect("source-bound oracle validation");
    let capabilities = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
    let mut config = InterpreterConfig::default();
    config.fixture.net_grants = usize::from(caps.net_allow.is_some()) + caps.net_grants.len();
    let result = witchy_wir::canonical_interpreter::interpret_program(&validated, function, capabilities, config).expect("opaque closure oracle");
    assert_eq!(result.output, expected);
    assert_eq!(result.leaked_objects, 0);
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, caps, 256).expect("opaque closure admitted Wasm");
    vm.run().expect("opaque closure Wasm execution");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
}

#[test]
fn carried_state_unchanged_expected_three_legs() {
    assert_opaque_closure_three_legs_with_caps(
        include_str!("../../examples/carried_state/src/carried_state.witchy"),
        &["ok: counted rows in users", "denied: secrets is outside this handle (scoped to users)"],
        // Carry a real Net token, but authorize no network destination.
        Capabilities { print: true, quiet: true, net_allow: Some(Vec::new()), ..Default::default() },
    );
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

fn make(own text: String, console: Console) -> fn(String) -> ():
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
fn canonical_layout_family_task_tuple_three_legs() {
    assert_opaque_closure_three_legs(r#"
import chan
async fn next(n: Int) -> Int:
    n + 1
async fn main(console: Console):
    let values = chan.par_map([4], fn(n): next(n)).await
    console.print("${values}")
"#, &["[5]"]);
}
#[test]
fn opaque_closure_mixed_string_console_after_original_cleanup_three_legs() {
    assert_opaque_closure_three_legs(r#"
fn prefix(n: Int) -> String:
    "log: " + "${n}"

fn make(own text: String, console: Console) -> fn(String) -> ():
    let log = fn(message: String): console.print(text + message)
    console.print(text)
    log

fn main(console: Console):
    let log = make(prefix(7), console)
    log(" A")
    log(" B")
"#, &["log: 7", "log: 7 A", "log: 7 B"]);
}
