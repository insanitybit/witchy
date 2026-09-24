//! Source-backed crypto extensions: independent answers and balanced ownership.
use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterConfig, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

fn both_modes(source: &str, expected: &[&str]) {
    for source in [source.to_owned(), format!("mode opt\n{source}")] {
        let checked = witchy::resolve_std_only_checked(&source).expect("crypto source checks");
        let output = witchy::interpreter::run_module_args(
            checked.module().clone(), ".", Vec::new(), Vec::new(),
        ).expect("source crypto executes");
        assert_eq!(output, expected, "source expectation");
        let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("crypto source admits");
        let entry = artifact.entry().expect("source entry");
        let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let validated = codegen::validate_checked_module_for_oracle(&checked).expect("independent validation");
        let arguments = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
        let oracle = interpret_program(&validated, function, arguments, InterpreterConfig {
            max_steps: 20_000_000_000, ..Default::default()
        }).expect("canonical crypto executes");
        assert_eq!(oracle.output, expected, "canonical expectation");
        assert_eq!(oracle.leaked_objects, 0, "canonical ownership balance");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
            print: true, quiet: true, ..Default::default()
        }, 256).expect("admitted crypto instance");
        vm.run().expect("Wasm crypto executes");
        assert_eq!(vm.output(), expected, "Wasm expectation");
        assert_eq!(vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"), 0);
    }
}

#[test]
fn crypto_extensions_shake_raw_vectors_prefixes_and_length_errors() {
    both_modes(r#"
import crypto
import bytes
import encoding

fn squeeze(input: Bytes, length: Int, strong: Bool) -> String:
    let result = if strong: crypto.shake256(input, length) else: crypto.shake128(input, length)
    match result:
        Ok(output) -> encoding.hex_encode_bytes(output)
        Err(_) -> "rejected"

fn replace_input(var input: Bytes, console: Console) -> Int:
    console.print("length-once")
    input = bytes.from_string("abc")
    16

fn main(console: Console):
    let empty = bytes.from_string("")
    console.print(squeeze(empty, 0, false))
    console.print(squeeze(empty, 16, false))
    console.print(squeeze(empty, 32, false))
    console.print(squeeze(empty, 16, true))
    console.print(squeeze(empty, 32, true))
    console.print(squeeze(empty, -1, false))
    console.print(squeeze(empty, 1048577, true))
    var octets: List(Int) = []
    for i in 0..200:
        list.push(octets, 163)
    let raw = bytes.from_list(octets).unwrap_or(empty)
    console.print(squeeze(raw, 32, false))
    console.print(squeeze(raw, 32, true))
    console.print("${bytes.length(raw)} ${bytes.at(raw, 0)}")
    var input = empty
    let snapshot = input
    match crypto.shake128(input, replace_input(input, console)):
        Ok(output) -> console.print(encoding.hex_encode_bytes(output))
        Err(_) -> console.print("unexpected")
    console.print("${bytes.length(snapshot)} ${encoding.hex_encode_bytes(input)}")
"#, &[
        "", "7f9c2ba4e88f827d616045507605853e",
        "7f9c2ba4e88f827d616045507605853ed73b8093f6efbc88eb1a6eacfa66ef26",
        "46b9dd2b0ba88d13233b3feb743eeb24",
        "46b9dd2b0ba88d13233b3feb743eeb243fcd52ea62b81b82b50c27646ed5762f",
        "rejected", "rejected",
        // NIST FIPS 202 examples: 1600-bit message, all octets A3.
        "131ab8d2b594946b9c81333f9bb6e0ce75c3b93104fa3469d3917457385da037",
        "cd8a920ed141aa0407a22d59288652e9d9f1a7ee0c1e7c1ca699424da84a904d",
        "200 163", "length-once", "7f9c2ba4e88f827d616045507605853e", "0 616263",
    ]);
}

