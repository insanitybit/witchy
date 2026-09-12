//! Whole-source fixture execution and its proof/terminal boundaries.
use super::*;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use witchy_testkit::{ConsoleFixture, FixtureFamily, FixtureOutcome, FixtureStep, FixtureValue, SourceLocation, U64Text};
use witchy_wir::canonical_interpreter::{self as oracle, InterpreterConfig, InterpreterValue, OracleConsoleEvent};

struct Files(std::path::PathBuf);
impl Files {
    fn new(source: &str) -> Self {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("witchy-console-fixture-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("suite.witchy"), source).unwrap();
        Self(path)
    }
    fn driver(&self) -> witchy_types::pipeline::CheckedTestDriverModule {
        let (suite, _) = crate::link_test_file(self.0.join("suite.witchy").to_str().unwrap()).unwrap();
        witchy_interp::pipeline::check_console_fixture_test_driver(&suite, "suite.test_console").unwrap()
    }
}
impl Drop for Files { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
fn plan(lines: &[&str]) -> FixturePlan {
    FixturePlan {
        console: Some(ConsoleFixture { script: lines.iter().map(|text| FixtureStep {
            operation: "print".into(), target: None,
            arguments: BTreeMap::from([("text".into(), FixtureValue::String((*text).into()))]),
            effective_rights: Some(vec!["Write".into()]),
            outcome: FixtureOutcome::Return { value: FixtureValue::Null }, required: true,
        }).collect() }),
        expectations: witchy_testkit::Expectations { require_complete_scripts: true, ..Default::default() },
        ..Default::default()
    }
}
fn source_outcome(driver: &witchy_types::pipeline::CheckedTestDriverModule, plan: &FixturePlan) -> witchy_testkit::TestTranscript {
    interpreter::run_checked_test_driver_fixtures(driver, plan.clone()).unwrap().transcript
}
fn run_oracle(driver: &witchy_types::pipeline::CheckedTestDriverModule, artifact: &witchy_wir::AdmittedCanonicalArtifact)
    -> (Result<oracle::ProgramRun, oracle::InterpretError>, Vec<OracleConsoleEvent>)
{
    let checked = codegen::validate_checked_test_driver_for_oracle(driver).unwrap();
    let entry = artifact.entry().unwrap();
    let function = witchy_wir::canonical_safewir::FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let trace = Arc::new(Mutex::new(Vec::new()));
    let result = oracle::interpret_entry_program(&checked, function,
        entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect(), &[],
        InterpreterConfig { console_trace: Some(trace.clone()), ..Default::default() });
    let events = trace.lock().unwrap().clone();
    (result, events)
}

#[test]
fn canonical_console_fixture_captured_console_executes_with_complete_parity() {
    let files = Files::new("fn test_console(console: Console):\n    let emit = fn():\n        console.print(\"captured\")\n    emit()\n");
    let driver = files.driver();
    let artifact = codegen::compile_checked_test_driver_artifact(&driver).expect_lowered("captured fixture Console admission");
    let plan = plan(&["captured"]);
    let source = source_outcome(&driver, &plan);
    let native = runtime::Runtime::batch().unwrap().run_admitted_fixtures(&artifact, plan, crate::RUN_MEMORY_PAGES).unwrap();
    assert_eq!(source.result, TestResult::Passed);
    assert_eq!(source, native.transcript, "captured Console full source/native transcript");
    assert_eq!(source.stdout, ["captured"]);
    assert_eq!(native.output, source.stdout);
    assert_eq!(native.leaked_objects, Some(0));
    assert_eq!(source.events.len(), 1);
    assert_eq!(source.events[0].effective_rights, ["Write"]);
    assert_eq!(source.events[0].source, Some(SourceLocation {
        module: "suite".into(), line: U64Text::new(3), column: U64Text::new(1),
    }));
    let (run, events) = run_oracle(&driver, &artifact);
    let run = run.unwrap();
    assert_eq!(run.output, source.stdout);
    assert_eq!(run.leaked_objects, 0);
    assert_eq!(events, [OracleConsoleEvent {
        text: "captured".into(), function: "suite.test_console".into(), line: 3,
    }]);
}

#[test]
fn canonical_console_fixture_source_admission_full_trace_and_zero_leaks() {
    let files = Files::new("fn helper(console: Console) -> String:\n    console.print(\"inner\")\n    \"outer\"\nfn main(console: Console):\n    console.print(\"unused main\")\nfn test_console(console: Console):\n    console.print(helper(console))\n    console.print(\"last\\n\")\n");
    let driver = files.driver();
    assert!(driver.canonical_source().unwrap().is_console_fixture());
    assert_eq!(driver.identity().unwrap().recipe_version(), 1);
    let main = driver.module().items.iter().find_map(|item| match item {
        ast::Item::Function(function) if function.name == "main" => Some(function), _ => None,
    }).unwrap();
    assert_eq!(main.ret, Some(ast::Type::Named("Nil".into(), Vec::new())));
    assert_ne!(driver.identity().unwrap().original_source_digest(), driver.identity().unwrap().derived_source_digest());
    let artifact = codegen::compile_checked_test_driver_artifact(&driver).expect_lowered("source fixture admission");
    assert_eq!(artifact.entry().unwrap().capabilities(), &[witchy_wir::CanonicalEntryCapability::Console]);
    let plan = plan(&["inner", "outer", "last"]);
    let source = source_outcome(&driver, &plan);
    let native = runtime::Runtime::batch().unwrap().run_admitted_fixtures(&artifact, plan, crate::RUN_MEMORY_PAGES).unwrap();
    assert_eq!(source, native.transcript, "complete source/native evidence");
    assert_eq!(source.result, TestResult::Passed);
    assert_eq!(source.stdout, ["inner", "outer", "last"]);
    assert_eq!(native.leaked_objects, Some(0));
    assert_eq!(source.events.len(), 3);
    for (event, line) in source.events.iter().zip([2, 7, 8]) {
        assert_eq!(event.family, FixtureFamily::Console);
        assert_eq!(event.operation, "print");
        assert_eq!(event.effective_rights, ["Write"]);
        assert_eq!(event.source, Some(SourceLocation { module: "suite".into(), line: U64Text::new(line), column: U64Text::new(1) }));
    }
    let (run, events) = run_oracle(&driver, &artifact);
    let run = run.unwrap();
    assert_eq!(run.output, source.stdout);
    assert_eq!(run.leaked_objects, 0);
    assert_eq!(events, vec![
        OracleConsoleEvent { text: "inner".into(), function: "suite.helper".into(), line: 2 },
        OracleConsoleEvent { text: "outer".into(), function: "suite.test_console".into(), line: 7 },
        OracleConsoleEvent { text: "last".into(), function: "suite.test_console".into(), line: 8 },
    ]);
}

#[test]
fn canonical_console_fixture_print_then_abort_preserves_effects_without_zero_leak_claim() {
    for message in ["boom", "runtime error: literal user message"] {
    let files = Files::new(&format!("fn test_console(console: Console):\n    console.print(\"before\")\n    fail(\"{message}\")\n"));
    let driver = files.driver();
    let artifact = codegen::compile_checked_test_driver_artifact(&driver).expect_lowered("abort fixture admission");
    let plan = plan(&["before"]);
    let source = source_outcome(&driver, &plan);
    let native = runtime::Runtime::batch().unwrap().run_admitted_fixtures(&artifact, plan, crate::RUN_MEMORY_PAGES).unwrap();
    assert_eq!(source, native.transcript);
    assert_eq!(native.leaked_objects, None, "an abort is not a reclamation proof");
    let expected = format!("`suite.test_console`, line 3: {message}");
    assert_eq!(source.result, TestResult::Failed { message: expected.clone() });
    assert_eq!(source.stdout, ["before"]);
    assert_eq!(source.events.len(), 1);
    let (run, events) = run_oracle(&driver, &artifact);
    assert_eq!(run.unwrap_err().to_string(), format!("runtime error: {expected}"));
    assert_eq!(events, [OracleConsoleEvent { text: "before".into(), function: "suite.test_console".into(), line: 2 }]);
    }
}

#[test]
fn canonical_console_fixture_refuses_missing_provider_and_unconsumed_transcript() {
    let files = Files::new("fn test_console(console: Console):\n    console.print(\"actual\")\n");
    let driver = files.driver();
    let artifact = codegen::compile_checked_test_driver_artifact(&driver).expect_lowered("fixture admission");
    let mut runtime = runtime::Runtime::batch().unwrap();
    let error = runtime.run_admitted_fixtures(&artifact, FixturePlan::default(), crate::RUN_MEMORY_PAGES).unwrap_err();
    assert!(error.to_string().contains("missing Console provider"));
    assert!(interpreter::run_checked_test_driver_fixtures(&driver, FixturePlan::default()).unwrap_err().to_string().contains("missing Console provider"));
    for plan in [plan(&["wrong"]), plan(&["actual", "unconsumed"])] {
        let source = source_outcome(&driver, &plan);
        let native = runtime.run_admitted_fixtures(&artifact, plan, crate::RUN_MEMORY_PAGES).unwrap();
        assert_eq!(source, native.transcript);
        assert!(!matches!(native.result, runtime::FixtureWasmResult::Passed));
        assert!(!matches!(source.result, TestResult::Passed));
    }
}

#[test]
fn canonical_console_fixture_recipe_refuses_other_types_and_providers() {
    for source in [
        "fn test_console(value: Int):\n    Nil\n",
        "type Holder:\n    console: Console\nfn test_console(value: Holder):\n    Nil\n",
        "fn test_console(console: Console) -> Int:\n    1\n",
    ] {
        let files = Files::new(source);
        let (suite, _) = crate::link_test_file(files.0.join("suite.witchy").to_str().unwrap()).unwrap();
        assert!(witchy_interp::pipeline::check_console_fixture_test_driver(&suite, "suite.test_console").is_err());
    }
    let files = Files::new("fn test_console(console: Console):\n    console.print(\"ok\")\n");
    let driver = files.driver();
    let artifact = codegen::compile_checked_test_driver_artifact(&driver).expect_lowered("fixture admission");
    let mut extra = plan(&["ok"]);
    extra.clock = Some(Default::default());
    let error = runtime::Runtime::batch().unwrap().run_admitted_fixtures(&artifact, extra, crate::RUN_MEMORY_PAGES).unwrap_err();
    assert!(error.to_string().contains("other providers"));
}

#[test]
fn canonical_console_fixture_cli_required_step_failure_overrides_successful_guest_return() {
    let files = Files::new("fn test_console(console: Console):\n    console.print(\"actual\")\n");
    let driver = files.driver();
    let plan = plan(&["actual", "unused"]);
    let expected = "required Console.print fixture step was not consumed";
    let source = interpreter::run_checked_test_driver_fixtures(&driver, plan.clone()).unwrap();
    assert!(matches!(source.result, interpreter::FixtureProgramResult::Passed { .. }),
        "the guest must return normally so only the fixture postcondition causes failure");
    assert_eq!(source.transcript.result, TestResult::Failed { message: expected.into() });
    assert_eq!(source.transcript.stdout, ["actual"]);
    assert_eq!(source.transcript.events.len(), 1);
    let plan_path = files.0.join("plan.json");
    std::fs::write(&plan_path, serde_json::to_vec(&plan).unwrap()).unwrap();
    for (name, backend) in [
        ("interpreter", TestBackend::Interpreter),
        ("wasm", TestBackend::Wasm),
        ("both", TestBackend::Both),
    ] {
        let result = run_fixture_test(&driver, &plan, backend).expect("no backend divergence");
        assert!(!result.passed, "{name} must reject unused required script steps");
        assert_eq!(result.error.as_deref(), Some(expected));
        assert_eq!(result.output, ["actual"]);
        assert_eq!(result.transcript, source.transcript);
        let options = TestOptions::parse([
            "--fixtures".to_owned(), plan_path.to_str().unwrap().to_owned(),
            "--backend".to_owned(), name.to_owned(),
            files.0.join("suite.witchy").to_str().unwrap().to_owned(),
        ]).unwrap();
        // This is the CLI decision consumed by dispatch: Ok(false) exits 1,
        // not success and not an infrastructure/compilation error (exit 2).
        assert_eq!(run_tests(&options), Ok(false), "{name} CLI status");
    }
}
