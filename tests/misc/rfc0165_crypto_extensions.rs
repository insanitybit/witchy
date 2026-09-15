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
fn crypto_extensions_hash_vectors_and_argument_snapshots() {
    let source = r#"
import crypto

fn first(key: String, console: Console) -> String:
    console.print("left")
    key

fn second(var key: String, console: Console) -> String:
    console.print("right")
    key = "ff"
    "Hi There"

fn main(console: Console):
    console.print(crypto.sha512("abc"))
    console.print(crypto.sha3_256("abc"))
    console.print(crypto.sha512(""))
    console.print(crypto.sha3_256(""))
    var key = "$KEY"
    let snapshot = key
    console.print(crypto.hmac_sha256(first(key, console), second(key, console)))
    console.print("${snapshot == "$KEY"} ${key == "ff"}")
    console.print(crypto.hmac_sha256("$PLUS", "Hi There"))
    console.print(crypto.hmac_sha256("", ""))
"#.replace("$KEY", &"0b".repeat(20)).replace("$PLUS", &"+b".repeat(20));
    // FIPS 180-4, FIPS 202, and RFC 4231 case 1. No backend supplies answers.
    both_modes(&source, &[
        "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
        "3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532",
        "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e",
        "a7ffc6f8bf1ed76651c14756a061d662f580ff4de43b49fa82d80a4b80f8434a",
        "left", "right",
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
        "true true",
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
        "b613679a0814d9ec772f95d778c35fc5ff1697c493715653c6c712144292c5ad",
    ]);
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

const KEY: &str = "048f81cd9fca785a42a6f5dd58972cc0f702e83b1c960b5912354471496597e227fec81ff1d52530b06d7091649e6beb49dba70968b4b727bb24e3ceb7dd01a039";
const SIGNATURE: &str = "304402203260029f4c6beb2e78afdd906c057c63f8828e2b03820de7053d97254577fb8c02204478b9b75f8fd7a1ce4298f0d119e12926dafda116ae4c197b0048dc117bc9de";

#[test]
fn crypto_extensions_ecdsa_raw_hex_statuses_and_plus_spelling() {
    let message = "webauthn-es256-test-message";
    let encoded: String = message.as_bytes().iter().map(|byte| format!("{byte:02x}")).collect();
    let plus_key = format!("+4{}", &KEY[2..]);
    let mut source = String::from("import crypto\nfn main(console: Console):\n");
    let cases = [
        (false, KEY, message, SIGNATURE, "true"),
        (true, KEY, encoded.as_str(), SIGNATURE, "true"),
        (false, plus_key.as_str(), message, SIGNATURE, "true"),
        (false, KEY, "tampered", SIGNATURE, "false"),
        (true, KEY, "+f", SIGNATURE, "false"),
        (true, "bad", "bad", "bad", "ECDSA P-256 public key is malformed"),
        (true, KEY, "gg", "bad", "ECDSA P-256 message is malformed"),
        (true, KEY, "", "bad", "ECDSA P-256 signature is malformed"),
        (false, KEY, message, "3006020180020101", "ECDSA P-256 signature is malformed"),
        (false, KEY, message, "300702020001020101", "ECDSA P-256 signature is malformed"),
        (false, KEY, message, "3006020100020100", "false"),
        (false, KEY, "", SIGNATURE, "false"),
    ];
    for (hex, key, message, signature, _) in cases {
        let operation = if hex { "ecdsa_p256_verify_hex" } else { "ecdsa_p256_verify" };
        source.push_str(&format!("    match crypto.{operation}(\"{key}\", \"{message}\", \"{signature}\"):\n        Ok(value) -> console.print(\"${{value}}\")\n        Err(error) -> console.print(crypto.verify_error_message(error))\n"));
    }
    both_modes(&source, &cases.iter().map(|case| case.4).collect::<Vec<_>>());
}
