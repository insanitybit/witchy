//! RFC 8032 known answers across source, independent oracle, and admitted Wasm.
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{InterpreterConfig, InterpreterValue, interpret_entry_program};

// https://www.rfc-editor.org/rfc/rfc8032#section-7.1, TEST 1 and TEST 2.
const KEY: &str = concat!("d75a980182b10ab7d54bfed3c964073a", "0ee172f3daa62325af021a68f707511a");
const SIGNATURE: &str = concat!("e5564300c360ac729086e2cc806e828a", "84877f1eb8e5d974d873e06522490155", "5fb8821590a33bacc61e39701cf9b46b", "d25bf5f0595bbe24655141438e7a100b");
const KEY_R: &str = concat!("3d4017c3e843895a92b70aa74d1b7ebc", "9c982ccf2ec4968cc0cd55f12af4660c");
const SIGNATURE_R: &str = concat!("92a009a9f0d4cab8720e820b5f642540", "a2b27b5416503f8fb3762223ebdb69da", "085ac1e43e15996e458f3613d0f11d8c", "387b2eaeb4302aeeb00d291612bb0c00");

#[test]
fn canonical_ed25519_known_answers_statuses_and_argument_snapshots_three_legs() {
    let source = format!(r#"import crypto
fn report(console: Console, result: Result(Bool, crypto.VerifyError)):
    match result:
        Ok(valid) -> console.print("${{valid}}")
        Err(error) -> console.print(crypto.verify_error_message(error))
fn replace(var key: String) -> String:
    key = "00"
    ""
fn main(console: Console):
    report(console, crypto.ed25519_verify("{KEY}", "", "{SIGNATURE}"))
    report(console, crypto.ed25519_verify("{KEY_R}", "r", "{SIGNATURE_R}"))
    report(console, crypto.ed25519_verify("{upper_key}", "", "{upper_signature}"))
    report(console, crypto.ed25519_verify("{KEY}", "tampered", "{SIGNATURE}"))
    report(console, crypto.ed25519_verify("bad", "", "bad"))
    report(console, crypto.ed25519_verify("{KEY}", "", "bad"))
    report(console, crypto.ed25519_verify("{plus_key}", "", "{SIGNATURE}"))
    var key = "{KEY}"
    report(console, crypto.ed25519_verify(key, replace(key), "{SIGNATURE}"))
    console.print(key)
"#, upper_key = KEY.to_uppercase(), upper_signature = SIGNATURE.to_uppercase(), plus_key = "+0".repeat(32));
    let expected = ["true", "true", "true", "false", "Ed25519 public key is malformed", "Ed25519 signature is malformed", "false", "true", "00"];
    let checked = witchy::resolve_std_only_checked(&source).unwrap();
    let root = crate::temp_dir::TempDir::new("canonical-ed25519");
    assert_eq!(witchy::interpreter::run_checked_module(&checked, root.path(), Vec::new()).unwrap(), expected);
    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(&checked, |validated, artifact| {
        let entry = artifact.entry().unwrap();
        let function = witchy_wir::canonical_safewir::FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let tokens = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
        let run = interpret_entry_program(validated, function, tokens, &[], InterpreterConfig::default()).unwrap();
        assert_eq!(run.output, expected);
        assert_eq!(run.leaked_objects, 0);
    }).expect_lowered("Ed25519 source reaches only its authenticated read-slice import");
    let mut runtime = Runtime::new().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities { print: true, ..Capabilities::none() }, 256).unwrap();
    vm.run().unwrap();
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
}

#[path = "rfc0165_crypto_hash.rs"]
mod hash;

#[path = "rfc0165_crypto_rsa.rs"]
mod rsa;
