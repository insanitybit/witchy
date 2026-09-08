//! RFC-0165 real-standard-library encoding reachability and execution slice.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterConfig, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

const HEX_ENCODE_SOURCE: &str = r#"
import encoding

fn main(console: Console):
    console.print(encoding.hex_encode(""))
    console.print(encoding.hex_encode("Hi"))
    console.print(encoding.hex_encode("é🙂"))
"#;

const HEX_DECODE_SOURCE: &str = r#"
import encoding

fn main(console: Console):
    match encoding.hex_decode("4869"):
        Ok(value) -> console.print(value)
        Err(_) -> console.print("decode error")
"#;

#[test]
fn real_std_hex_encode_agrees_with_oracle_and_admitted_wasm() {
    let checked = witchy::resolve_std_only_checked(HEX_ENCODE_SOURCE).expect("real std hex source checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("real std hex_encode must admit");
    let source_output = witchy::interpreter::run_module_args(checked.module().clone(), ".", Vec::new(), Vec::new())
        .expect("source interpreter runs the real std checked module");
    assert_eq!(source_output, vec!["", "4869", "c3a9f09f9982"], "source interpreter expected output");
    let entry = artifact.entry().expect("hex_encode program entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked).expect("hex source validates for oracle");
    let arguments = entry
        .capabilities()
        .iter()
        .copied()
        .map(InterpreterValue::entry_capability)
        .collect();
    let oracle = interpret_program(
        &validated,
        function,
        arguments,
        InterpreterConfig { max_steps: 20_000_000_000, ..Default::default() },
    )
    .expect("canonical oracle runs real std hex_encode");
    assert_eq!(oracle.output, source_output, "canonical oracle must match source interpreter");
    assert_eq!(oracle.leaked_objects, 0, "source oracle must not leak");

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, quiet: true, ..Default::default() }, 256)
        .expect("spawn admitted real std hex_encode");
    vm.run().expect("admitted Wasm runs real std hex_encode");
    assert_eq!(vm.output(), source_output, "admitted Wasm must match source interpreter");
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"), 0);
}

#[test]
fn reachable_std_hex_decode_specialization_still_refuses() {
    let checked = witchy::resolve_std_only_checked(HEX_DECODE_SOURCE).expect("real std hex source checks");
    let canonical = checked.canonical_source().expect("canonical source");
    let map_ok_name = canonical
        .typed_module()
        .module()
        .items
        .iter()
        .find_map(|item| match item {
            witchy_syntax::ast::Item::Function(function) if function.name.starts_with("Result__map_ok__") => Some(function.name.clone()),
            _ => None,
        })
        .expect("real std hex_decode must monomorphize Result.map_ok");
    assert!(canonical.callable_signature(&map_ok_name).is_some(), "reachable map_ok must be in the runtime inventory");
    assert!(!canonical.obligations().iter().any(|obligation| {
        matches!(obligation, witchy_types::pipeline::CanonicalObligation::Specialization(name) if name.starts_with("Result__map_ok__"))
    }), "reachable map_ok must not be hidden as an unreachable obligation");
    let outcome = codegen::compile_checked_module_artifact(&checked);
    match outcome {
        codegen::LoweringOutcome::Unsupported(reason) => {
            let refusal = reason.to_string();
            // Scoped to the currently unsupported decode helper; migrate this
            // pin with intentional hex-decode support rather than weakening it.
            assert_eq!(
                refusal,
                "unsupported lowering: canonical SafeWIR lowering failed: MissingBinding(\"encoding.hex_decode_bytes_raw\")"
            );
        }
        codegen::LoweringOutcome::Lowered(_) => panic!("reachable unsupported decode unexpectedly admitted"),
        codegen::LoweringOutcome::Rejected(error) => panic!("reachable unsupported decode was rejected as invalid: {error}"),
    }
}
