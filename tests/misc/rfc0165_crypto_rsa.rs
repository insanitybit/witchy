//! Bounded RSA verification and raw-byte hashing through all three execution legs.
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{InterpreterConfig, InterpreterValue, interpret_entry_program};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn agree(source: &str, expected: &[&str]) {
    let checked = witchy::resolve_std_only_checked(source).unwrap();
    let root = crate::temp_dir::TempDir::new("canonical-rsa-bytes");
    assert_eq!(witchy::interpreter::run_checked_module(&checked, root.path(), Vec::new()).unwrap(), expected);
    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(&checked, |validated, artifact| {
        let entry = artifact.entry().unwrap();
        let function = witchy_wir::canonical_safewir::FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let tokens = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
        let run = interpret_entry_program(validated, function, tokens, &[], InterpreterConfig::default()).unwrap();
        assert_eq!(run.output, expected);
        assert_eq!(run.leaked_objects, 0);
    }).expect_lowered("crypto uses authenticated initialized bounded slices");
    let mut runtime = Runtime::new().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities { print: true, ..Capabilities::none() }, 256).unwrap();
    vm.run().unwrap();
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
}

#[test]
fn canonical_crypto_rsa_statuses_and_argument_snapshots_three_legs() {
    use aws_lc_rs::signature::KeyPair;
    let key = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).unwrap();
    let mut signature = vec![0; key.public_modulus_len()];
    key.sign(&aws_lc_rs::signature::RSA_PKCS1_SHA256, &aws_lc_rs::rand::SystemRandom::new(), b"", &mut signature).unwrap();
    let key = hex(key.public_key().as_ref());
    let signature = hex(&signature);
    let source = format!(r#"import crypto
fn report(console: Console, result: Result(Bool, crypto.VerifyError)):
    match result:
        Ok(valid) -> console.print("${{valid}}")
        Err(error) -> console.print("malformed")
fn replace(console: Console, var key: String) -> String:
    console.print("replace")
    key = "00"
    ""
fn main(console: Console):
    report(console, crypto.rsa_pkcs1_sha256_verify("{key}", "", "{signature}"))
    report(console, crypto.rsa_pkcs1_sha256_verify("{upper_key}", "", "{upper_signature}"))
    report(console, crypto.rsa_pkcs1_sha256_verify("{key}", "tampered", "{signature}"))
    report(console, crypto.rsa_pkcs1_sha256_verify("bad", "", "bad"))
    report(console, crypto.rsa_pkcs1_sha256_verify("{key}", "", "bad"))
    report(console, crypto.rsa_pkcs1_sha256_verify("{key}", "", ""))
    var original = "{key}"
    report(console, crypto.rsa_pkcs1_sha256_verify(original, replace(console, original), "{signature}"))
    console.print(original)
"#, upper_key = key.to_uppercase(), upper_signature = signature.to_uppercase());
    agree(&source, &["true", "true", "false", "malformed", "malformed", "malformed", "replace", "true", "00"]);
}

