//! Checked source signing through admitted Wasm and explicit, independent
//! oracle fixtures. No clock, keychain, filesystem, or network is consulted.
use witchy::runtime::{Capabilities, Runtime, SecretGrant};
use witchy_wir::canonical_interpreter::{
    interpret_entry_program, interpret_program, InterpretError, InterpreterConfig,
    InterpreterValue, OracleSecretFixture, OracleSecretStoreFixture,
};
use witchy_wir::canonical_safewir::FunctionId;

// RFC 8032 vector 1, also used by the native registry-host boundary tests.
// Expected output is fixed independently of both execution implementations.
const SEED: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
const PUBLIC_KEY: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
const SIGNATURE: &str = concat!(
    "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555f",
    "b8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
);

fn seed() -> [u8; 32] {
    (0..SEED.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&SEED[index..index + 2], 16).unwrap())
        .collect::<Vec<_>>()
        .try_into()
        .unwrap()
}

fn oracle_config(signing: bool) -> InterpreterConfig {
    let mut config = InterpreterConfig::default();
    // An explicit empty Store is a valid grant, not an absent provider.
    config.fixture.secret_store = Some(OracleSecretStoreFixture {
        secrets: if signing {
            vec![("signing".into(), OracleSecretFixture::Signing(seed().to_vec()))]
        } else {
            Vec::new()
        },
    });
    config
}

#[test]
fn registry_hosts_checked_signing_and_nullable_lookup_agree() {
    let source = r#"import crypto
import secretstore
fn main(console: Console, secrets: SecretStore):
    let key = secrets.require("signing")
    let public_key = fn(): crypto.public_key(key)
    let signature = crypto.sign(key, "")
    console.print(public_key())
    console.print(signature)
    match secrets.get("absent"):
        Some(found) -> console.print("unexpected secret")
        None -> console.print("missing")
    console.print(signature)
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("checked signing source");
    let expected = [PUBLIC_KEY, SIGNATURE, "missing", SIGNATURE];
    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            let entry = artifact.entry().expect("source-bound entry");
            let function = FunctionId::new(
                entry.export().strip_prefix('f').unwrap().parse().unwrap(),
            );
            for with_argv in [false, true] {
                let arguments = entry.capabilities().iter().copied()
                    .map(InterpreterValue::entry_capability).collect();
                let result = if with_argv {
                    interpret_entry_program(validated, function, arguments, &[], oracle_config(true))
                } else {
                    interpret_program(validated, function, arguments, oracle_config(true))
                }.expect("explicit signing fixture executes");
                assert_eq!(result.output, expected);
                assert_eq!(result.leaked_objects, 0);
            }
        },
    ).expect_lowered("checked signing source independently admits");

    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
        print: true,
        quiet: true,
        signing_key: Some(seed()),
        secrets: vec![SecretGrant {
            name: "signing".into(), bytes: seed().to_vec(), sealed: true,
        }],
        ..Capabilities::none()
    }, 256).expect("instantiate admitted signing artifact");
    vm.run().expect("native signing executes");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
}

#[test]
fn registry_hosts_checked_missing_required_secret_preserves_prior_output() {
    let source = r#"import crypto
import secretstore
fn main(console: Console, secrets: SecretStore):
    console.print("before")
    let key = secrets.require("missing")
    console.print(crypto.public_key(key))
    console.print("after")
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("checked required-secret source");
    let expected_error = "required secret `missing` was not granted";
    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            let entry = artifact.entry().expect("source-bound entry");
            let function = FunctionId::new(
                entry.export().strip_prefix('f').unwrap().parse().unwrap(),
            );
            for with_argv in [false, true] {
                let arguments = entry.capabilities().iter().copied()
                    .map(InterpreterValue::entry_capability).collect();
                let result = if with_argv {
                    interpret_entry_program(validated, function, arguments, &[], oracle_config(false))
                } else {
                    interpret_program(validated, function, arguments, oracle_config(false))
                };
                match result.expect_err("missing required secret must abort") {
                    InterpretError::DiagnosticAbort { message, output } => {
                        assert!(message.contains(expected_error), "{message}");
                        assert_eq!(output, ["before"]);
                    }
                    error => panic!("expected source diagnostic, got {error:?}"),
                }
            }
        },
    ).expect_lowered("missing secret is an execution failure, not admission refusal");

    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
        print: true, quiet: true, ..Capabilities::none()
    }, 256).expect("instantiate admitted empty-store artifact");
    // Root minting occurs during run; an empty Store must reach require's
    // source diagnostic rather than fail as an absent root during launch.
    let error = format!("{:#}", vm.run().expect_err("missing secret must abort"));
    assert!(error.contains(expected_error), "{error}");
    assert_eq!(vm.output(), ["before"]);
    // Abort does not establish successful cleanup: no zero-leak claim here.
}
