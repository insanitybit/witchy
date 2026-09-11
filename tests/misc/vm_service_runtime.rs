//! Frozen service ABI integration: source output alone cannot prove isolation.

#[test]
fn two_requests_use_one_isolated_service_and_retire_its_state() {
    use witchy::{codegen, runtime::{Capabilities, Runtime}};
    use witchy_wir::canonical_interpreter::{interpret_program, InterpreterValue};
    use witchy_wir::canonical_safewir::FunctionId;

    let template = r#"
import vm
import bytes

fn handler(state: Bytes, request: Bytes) -> Bytes:
    CALLBACK_BODY

fn main(console: Console):
    let states = vm.serve(bytes.from_string(""), [bytes.from_string("a"), bytes.from_string("b")], handler)
    for state in states:
        console.print(bytes.to_string(state))
"#;
    let cases = [
        ("accumulating", "state.concat(request)", ["a", "ab"], Some(1)),
        ("literal result", "bytes.from_string(\"fixed\")", ["fixed", "fixed"], None),
        ("input alias", "request", ["a", "b"], Some(1)),
        ("own input alias", "request", ["a", "b"], Some(1)),
    ];
    let mut runtime = Runtime::batch().unwrap();
    for (case, body, expected, expected_rc_roots) in cases {
        let source = template.replace("CALLBACK_BODY", body);
        let source = if case == "own input alias" {
            source.replace("fn handler(state: Bytes, request: Bytes)", "fn handler(own state: Bytes, own request: Bytes)")
        } else { source };
        let checked = witchy::resolve_std_only_checked(&source).unwrap();
        assert_eq!(witchy::interpreter::run_checked_module(&checked, ".", Vec::new()).unwrap(), expected,
            "{case}: source interpreter");
        let artifact = codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("isolated service admission");
        assert_eq!(artifact.vm_services().len(), 1, "{case}: admitted site");

        // Independent SafeWIR execution, using the established oracle entry
        // construction. Neither source output nor the runtime supplies this
        // result. Oracle service close releases its own final state and audits
        // the child heap as well as the parent outcome below.
        let entry = artifact.entry().expect("admitted entry");
        let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let arguments = entry.capabilities().iter().copied()
            .map(InterpreterValue::entry_capability).collect();
        let validated = codegen::validate_checked_module_for_oracle(&checked)
            .expect("independent canonical oracle validation");
        let oracle = interpret_program(&validated, function, arguments, Default::default())
            .unwrap_or_else(|error| panic!("{case}: canonical oracle: {error:?}"));
        assert_eq!(oracle.output, expected, "{case}: canonical oracle output");
        assert_eq!(oracle.leaked_objects, 0, "{case}: canonical oracle ownership");

        let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
            print: true, quiet: true, ..Default::default()
        }, 256).unwrap();
        vm.run().unwrap_or_else(|error| panic!("{case}: isolated Wasm service: {error:#}"));
        assert_eq!(vm.output(), expected, "{case}: Wasm output");
        let metrics = vm.vm_service_metrics();
        assert_eq!(metrics.started, 1, "{case}: worker count");
        assert_eq!(metrics.successful_steps, 2, "{case}: ordered requests");
        assert_eq!(metrics.isolated_memories, 1, "{case}: actual memory isolation");
        assert_eq!(metrics.closed, 1, "{case}: service close");
        assert_eq!(metrics.failed, 0, "{case}: service failure");
        assert_eq!(metrics.live_workers, 0, "{case}: worker cleanup");
        assert_eq!(metrics.live_state_roots, 0, "{case}: state cleanup");
        if let Some(expected) = expected_rc_roots {
            assert_eq!(metrics.retired_state_roots, expected, "{case}: final RC root");
        } else {
            // A literal may remain StaticData (zero RC roots) or be copied
            // into owned Bytes (one). Do not infer its physical storage from
            // source spelling; the runtime classifies the actual result.
            assert!(metrics.retired_state_roots <= 1, "{case}: {metrics:?}");
        }
        assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0, "{case}: parent Wasm heap");
        // Store destruction retires the final child root. The parent Wasm
        // heap audit is deliberately not a claim about child heap leaks.
    }
}
