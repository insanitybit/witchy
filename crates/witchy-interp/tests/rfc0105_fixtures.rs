#![cfg(feature = "test-fixtures")]

use std::collections::BTreeMap;

use witchy_interp::interpreter::{
    FixtureProgramResult, run_module_fixtures,
};
use witchy_syntax::parser::parse_module;
use witchy_testkit::{
    ClockFixture, ConsoleFixture, EnvFixture, Expectations, FixtureFamily,
    FixturePlan, RandFixture, TestResult, U64Text,
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
