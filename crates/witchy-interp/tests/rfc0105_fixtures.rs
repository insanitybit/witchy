#![cfg(all(feature = "test-fixtures", feature = "raw-module-test-api"))]

use std::collections::BTreeMap;

use witchy_interp::interpreter::{
    FixtureProgramResult, run_module_fixtures,
};
use witchy_syntax::parser::parse_module;
use witchy_testkit::{
    ClockFixture, ConsoleFixture, EnvFixture, Expectations, FetchFixture,
    FilesystemEntry, FilesystemFixture, FixtureErrorCode, FixtureFailure,
    FixtureFamily, FixtureOutcome, FixturePlan, FixtureStep, FixtureValue,
    RandFixture, TestResult, U64Text,
};

fn basic_plan() -> FixturePlan {
    FixturePlan {
        version: 1,
        console: Some(ConsoleFixture::default()),
        clock: Some(ClockFixture {
            start_ns: Some(U64Text::new(2_000_000)),
            step_ns: Some(U64Text::new(1_000_000)),
            repeat_last: false,
            script: Vec::new(),
        }),
        rand: Some(RandFixture {
            seed: Some(U64Text::new(7)),
            script: Vec::new(),
        }),
        env: Some(EnvFixture {
            values: BTreeMap::from([
                ("NAME".to_owned(), "fixture".to_owned()),
                ("HIDDEN".to_owned(), "ambient".to_owned()),
            ]),
            allow: vec!["NAME".to_owned()],
            script: Vec::new(),
        }),
        argv: Some(vec!["argument".to_owned()]),
        expectations: Expectations::default(),
        ..FixturePlan::default()
    }
}

#[test]
fn basic_roots_use_only_the_fixture_host_and_emit_one_transcript() {
    let source = r#"
fn main(console: Console, clock: Clock, rand: Rand, env: Env, args: List(String)) -> Int:
    let narrowed = env.only(["NAME"])
    match narrowed.get_env("NAME"):
        Some(value) -> console.print(value)
        None -> console.print("missing")
    console.print(list.at(args, 0))
    console.print("${rand.rand_u64()}")
    clock.now()
"#;
    let module = parse_module(source).expect("parse fixture program");
    let outcome =
        run_module_fixtures(module, basic_plan()).expect("run fixture program");
    match outcome.result {
        FixtureProgramResult::Passed { output, exit_code } => {
            assert_eq!(&output[..2], ["fixture", "argument"]);
            assert_eq!(output.len(), 3);
            assert_eq!(exit_code, 2);
        }
        FixtureProgramResult::Failed { error, .. } => {
            panic!("fixture program failed: {error}")
        }
    }
    assert_eq!(outcome.transcript.result, TestResult::Passed);
    for family in [
        FixtureFamily::Console,
        FixtureFamily::Clock,
        FixtureFamily::Rand,
        FixtureFamily::Env,
        FixtureFamily::Argv,
    ] {
        assert!(
            outcome
                .transcript
                .events
                .iter()
                .any(|event| event.family == family),
            "missing {family:?} event"
        );
    }
}

#[test]
fn undeclared_root_fails_before_ambient_authority_is_used() {
    let module = parse_module("fn main(clock: Clock) -> Int:\n    clock.now()\n")
        .expect("parse fixture program");
    let outcome = run_module_fixtures(
        module,
        FixturePlan {
            version: 1,
            expectations: Expectations::default(),
            ..FixturePlan::default()
        },
    )
    .expect("fixture execution has a transcript");
    match outcome.result {
        FixtureProgramResult::Failed { error, .. } => {
            assert!(
                error.message.contains("declared no `Clock` provider"),
                "{}",
                error.message
            );
        }
        FixtureProgramResult::Passed { .. } => {
            panic!("undeclared Clock must not run")
        }
    }
    assert!(outcome.transcript.events.is_empty());
    assert!(matches!(
        outcome.transcript.result,
        TestResult::Failed { .. }
    ));
}

#[test]
fn filesystem_fixture_is_shared_across_dir_and_file_handles() {
    let module = parse_module(
        "fn main(console: Console, dir: Dir[Read, Write]):\n    console.print(dir.read(\"input.txt\"))\n    let output = dir.write_file(\"output.txt\")\n    output.write(\"created\")\n    console.print(dir.read(\"output.txt\"))\n    dir.append(\"output.txt\", \"!\")\n    console.print(dir.read(\"output.txt\"))\n",
    )
    .expect("parse filesystem fixture program");
    let outcome = run_module_fixtures(
        module,
        FixturePlan {
            version: 1,
            console: Some(ConsoleFixture::default()),
            filesystem: Some(FilesystemFixture {
                entries: BTreeMap::from([(
                    "input.txt".to_owned(),
                    FilesystemEntry::File {
                        hex: "68656c6c6f".to_owned(),
                    },
                )]),
                rights: vec!["Read".to_owned(), "Write".to_owned()],
                entry_policy: None,
                script: Vec::new(),
            }),
            expectations: Expectations::default(),
            ..FixturePlan::default()
        },
    )
    .expect("run filesystem fixture");
    match outcome.result {
        FixtureProgramResult::Passed { output, .. } => {
            assert_eq!(output, ["hello", "created", "created!"]);
        }
        FixtureProgramResult::Failed { error, .. } => {
            panic!("filesystem fixture failed: {error}")
        }
    }
    assert!(
        outcome
            .transcript
            .events
            .iter()
            .filter(|event| event.family == FixtureFamily::Filesystem)
            .count()
            >= 7
    );
}

#[test]
fn fetch_fixture_returns_raw_responses_and_provider_error_sentinels() {
    let success_url = "https://example.com/data";
    let timeout_url = "https://example.com/slow";
    let request_step = |url: &str, outcome: FixtureOutcome| FixtureStep {
        operation: "fetch_send_len".to_owned(),
        target: Some(url.to_owned()),
        arguments: BTreeMap::from([
            ("method".to_owned(), FixtureValue::String("GET".to_owned())),
            ("headers".to_owned(), FixtureValue::List(Vec::new())),
            ("body".to_owned(), FixtureValue::Bytes(String::new())),
        ]),
        effective_rights: Some(vec!["https://example.com:443".to_owned()]),
        outcome,
        required: true,
    };
    let success = FixtureOutcome::Return {
        value: FixtureValue::Map(BTreeMap::from([
            ("status".to_owned(), FixtureValue::String("200".to_owned())),
            ("headers".to_owned(), FixtureValue::List(Vec::new())),
            ("body".to_owned(), FixtureValue::Bytes("66697874757265".to_owned())),
        ])),
    };
    let timeout = FixtureOutcome::Fail {
        error: FixtureFailure {
            code: FixtureErrorCode::Timeout,
            message: "configured timeout".to_owned(),
        },
    };
    let module = parse_module(&format!(
        "fn main(console: Console, fetch: Fetch):\n    console.print(fetch.send_raw(\"GET\", \"{success_url}\", \"\", \"\"))\n    console.print(fetch.send_raw(\"GET\", \"{timeout_url}\", \"\", \"\"))\n"
    ))
    .expect("parse Fetch fixture program");
    let outcome = run_module_fixtures(
        module,
        FixturePlan {
            version: 1,
            console: Some(ConsoleFixture::default()),
            fetch: Some(FetchFixture {
                origins: vec!["https://example.com:443".to_owned()],
                script: vec![
                    request_step(success_url, success),
                    request_step(timeout_url, timeout),
                ],
            }),
            expectations: Expectations::default(),
            ..FixturePlan::default()
        },
    )
    .expect("run Fetch fixture");
    match outcome.result {
        FixtureProgramResult::Passed { output, .. } => {
            assert!(output[0].starts_with("HTTP/1.1 200\r\n\r\nfixture"));
            assert_eq!(
                output[1],
                "WITCHY_FETCH_ERROR:timeout:configured timeout"
            );
        }
        FixtureProgramResult::Failed { error, .. } => {
            panic!("Fetch fixture failed: {error}")
        }
    }
    assert_eq!(
        outcome
            .transcript
            .events
            .iter()
            .filter(|event| event.family == FixtureFamily::Fetch)
            .count(),
        3
    );
}

#[test]
fn secret_fixtures_keep_material_opaque_and_preserve_scripted_crypto() {
    let reveal_module = parse_module(
        "import crypto\nimport secretstore\n\nfn main(console: Console, secrets: SecretStore):\n    let token = secretstore.require(secrets, \"token\")\n    console.print(crypto.reveal(token))\n",
    )
    .expect("parse reveal fixture program");
    let reveal = run_module_fixtures(
        reveal_module,
        FixturePlan {
            version: 1,
            console: Some(ConsoleFixture::default()),
            secrets: Some(witchy_testkit::SecretStoreFixture {
                entries: BTreeMap::from([(
                    "token".to_owned(),
                    witchy_testkit::SecretFixture {
                        hex: "666978747572652d746f6b656e".to_owned(),
                        usage: witchy_testkit::SecretUsage::Revealable,
                    },
                )]),
                script: Vec::new(),
            }),
            expectations: Expectations::default(),
            ..FixturePlan::default()
        },
    )
    .expect("run reveal fixture");
    match reveal.result {
        FixtureProgramResult::Passed { output, .. } => {
            assert_eq!(output, vec!["fixture-token"]);
        }
        FixtureProgramResult::Failed { error, .. } => {
            panic!("secret reveal fixture failed: {error}")
        }
    }

    let lookup = FixtureStep {
        operation: "secretstore_lookup".to_owned(),
        target: Some("signing".to_owned()),
        arguments: BTreeMap::new(),
        effective_rights: None,
        outcome: FixtureOutcome::Return {
            value: FixtureValue::String("Secret".to_owned()),
        },
        required: true,
    };
    let sign = FixtureStep {
        operation: "crypto.sign".to_owned(),
        target: None,
        arguments: BTreeMap::from([(
            "message".to_owned(),
            FixtureValue::String("payload".to_owned()),
        )]),
        effective_rights: None,
        outcome: FixtureOutcome::Return {
            value: FixtureValue::String("fixture-signature".to_owned()),
        },
        required: true,
    };
    let public_key = FixtureStep {
        operation: "crypto.public_key".to_owned(),
        target: None,
        arguments: BTreeMap::new(),
        effective_rights: None,
        outcome: FixtureOutcome::Return {
            value: FixtureValue::String("fixture-public-key".to_owned()),
        },
        required: true,
    };
    let scripted_module = parse_module(
        "import crypto\nimport secretstore\n\nfn main(console: Console, secrets: SecretStore):\n    let signing = secretstore.require(secrets, \"signing\")\n    console.print(crypto.sign(signing, \"payload\"))\n    console.print(crypto.public_key(signing))\n",
    )
    .expect("parse scripted crypto fixture program");
    let scripted = run_module_fixtures(
        scripted_module,
        FixturePlan {
            version: 1,
            console: Some(ConsoleFixture::default()),
            secrets: Some(witchy_testkit::SecretStoreFixture {
                entries: BTreeMap::from([(
                    "signing".to_owned(),
                    witchy_testkit::SecretFixture {
                        hex: "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
                            .to_owned(),
                        usage: witchy_testkit::SecretUsage::Signing,
                    },
                )]),
                script: vec![lookup, sign, public_key],
            }),
            expectations: Expectations::default(),
            ..FixturePlan::default()
        },
    )
    .expect("run scripted crypto fixture");
    match scripted.result {
        FixtureProgramResult::Passed { output, .. } => {
            assert_eq!(
                output,
                vec!["fixture-signature", "fixture-public-key"]
            );
        }
        FixtureProgramResult::Failed { error, .. } => {
            panic!("scripted crypto fixture failed: {error}")
        }
    }

    let use_only_module = parse_module(
        "import crypto\nimport secretstore\n\nfn main(secrets: SecretStore):\n    crypto.reveal(secretstore.require(secrets, \"token\"))\n",
    )
    .expect("parse sealed fixture program");
    let sealed = run_module_fixtures(
        use_only_module,
        FixturePlan {
            version: 1,
            secrets: Some(witchy_testkit::SecretStoreFixture {
                entries: BTreeMap::from([(
                    "token".to_owned(),
                    witchy_testkit::SecretFixture {
                        hex: "736563726574".to_owned(),
                        usage: witchy_testkit::SecretUsage::Sealed,
                    },
                )]),
                script: Vec::new(),
            }),
            expectations: Expectations::default(),
            ..FixturePlan::default()
        },
    )
    .expect("run sealed fixture");
    match sealed.result {
        FixtureProgramResult::Passed { .. } => {
            panic!("sealed secret was unexpectedly revealed")
        }
        FixtureProgramResult::Failed { error, .. } => {
            assert!(
                error
                    .message
                    .contains(witchy_caps::capabilities::SEALED_SECRET_REVEAL_ERROR)
            );
        }
    }
}

#[test]
fn exec_fixture_requires_attenuated_exec_and_fixture_dir_authority() {
    let module = parse_module(
        "fn main(console: Console, tools: Dir[Read], runner: Exec):\n    let echo = runner.only([\"echo\"])\n    console.print(echo.exec(tools, \"echo\", \"hello\\0world\", \"input\"))\n",
    )
    .expect("parse Exec fixture program");
    let run = FixtureStep {
        operation: "exec_run".to_owned(),
        target: Some("echo".to_owned()),
        arguments: BTreeMap::from([
            (
                "args".to_owned(),
                FixtureValue::List(vec![
                    FixtureValue::String("hello".to_owned()),
                    FixtureValue::String("world".to_owned()),
                ]),
            ),
            (
                "stdin".to_owned(),
                FixtureValue::String("input".to_owned()),
            ),
        ]),
        effective_rights: None,
        outcome: FixtureOutcome::Return {
            value: FixtureValue::Map(BTreeMap::from([
                (
                    "exit_code".to_owned(),
                    FixtureValue::String("7".to_owned()),
                ),
                (
                    "stdout".to_owned(),
                    FixtureValue::String("fixture stdout".to_owned()),
                ),
                (
                    "stderr".to_owned(),
                    FixtureValue::String("fixture stderr".to_owned()),
                ),
            ])),
        },
        required: true,
    };
    let outcome = run_module_fixtures(
        module,
        FixturePlan {
            version: 1,
            console: Some(ConsoleFixture::default()),
            filesystem: Some(FilesystemFixture {
                entries: BTreeMap::from([(
                    "echo".to_owned(),
                    FilesystemEntry::File {
                        hex: String::new(),
                    },
                )]),
                rights: vec!["Read".to_owned()],
                entry_policy: None,
                script: Vec::new(),
            }),
            exec: Some(witchy_testkit::ExecFixture {
                tools: vec!["echo".to_owned()],
                script: vec![run],
            }),
            expectations: Expectations::default(),
            ..FixturePlan::default()
        },
    )
    .expect("run Exec fixture");
    match outcome.result {
        FixtureProgramResult::Passed { output, .. } => {
            assert_eq!(output, vec!["7\nfixture stdoutfixture stderr"]);
        }
        FixtureProgramResult::Failed { error, .. } => {
            panic!("Exec fixture failed: {error}")
        }
    }
    let exec_run = outcome
        .transcript
        .events
        .iter()
        .find(|event| event.operation == "exec_run")
        .expect("exec_run transcript event");
    assert!(
        exec_run
            .effective_rights
            .contains(&"exec:echo".to_owned())
    );
    assert!(exec_run.effective_rights.contains(&"dir:Read".to_owned()));

    let widening_module = parse_module(
        "fn main(runner: Exec):\n    runner.only([\"bin/sh\"])\n",
    )
    .expect("parse Exec widening program");
    let widening = run_module_fixtures(
        widening_module,
        FixturePlan {
            version: 1,
            exec: Some(witchy_testkit::ExecFixture {
                tools: vec!["echo".to_owned()],
                script: Vec::new(),
            }),
            expectations: Expectations::default(),
            ..FixturePlan::default()
        },
    )
    .expect("run Exec widening fixture");
    match widening.result {
        FixtureProgramResult::Passed { .. } => {
            panic!("Exec fixture attenuation widened its allow-list")
        }
        FixtureProgramResult::Failed { error, .. } => {
            assert!(error.message.contains("cannot widen"));
        }
    }
}
