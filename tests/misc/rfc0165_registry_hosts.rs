//! Checked source signing through admitted Wasm and explicit, independent
//! oracle fixtures. No clock, keychain, filesystem, or network is consulted.
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{
    interpret_entry_program, interpret_program, InterpretError, InterpreterConfig,
    InterpreterValue, OracleSecretFixture, OracleSecretStoreFixture,
};
use witchy_wir::canonical_safewir::FunctionId;

// RFC 8032 vector 1, also used by the native registry-host boundary tests.
// Expected output is fixed independently of both execution implementations.
const SEED: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";

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
