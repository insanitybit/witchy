//! The `witchy test` runner: test discovery, integration-test grant
//! validation, dependency-root resolution, and execution on the compiled
//! backend. Extracted from the composition root.

use crate::commands::execution::run_checked_compiled;
use crate::{
    ast, codegen, enforce_performance_modes, interpreter, is_entry_function,
    link_test_file, parser, run_wasm_test_bytes, runtime, typeck,
};
use witchy_testkit::{FixturePlan, TestResult, TestTranscript};

/// Run a program on BOTH backends — the tree-walking interpreter and compiled
/// WebAssembly — and confirm they produce identical output. Witchy's
/// dual-backend equivalence is normally an internal test invariant; `witchy
/// verify` surfaces it as a guarantee you can check on your own code.
/// A failed in-language test: its (qualified) name and the abort message.
pub(crate) type TestFailure = (String, String);

pub(crate) const TEST_USAGE: &str =
    "usage: witchy test [--fixtures <plan.json>] [--backend interpreter|wasm|both] [--filter <text>] [--list] [--show-output] [--seed <u64>] [--format human|json] [--integration] [--dir <root>]... [--net <addr>]... <file.witchy|dir>";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TestBackend {
    Interpreter,
    Wasm,
    Both,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TestOutputFormat {
    Human,
    Json,
}

#[derive(Clone, Debug, Default)]
struct TestGrants {
    dir_roots: Vec<std::path::PathBuf>,
    net_allow: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct TestOptions {
    path: String,
    integration: bool,
    grants: TestGrants,
    fixture_path: Option<std::path::PathBuf>,
    backend: TestBackend,
    filter: Option<String>,
    list: bool,
    show_output: bool,
    seed: Option<u64>,
    format: TestOutputFormat,
}

impl TestOptions {
    pub(crate) fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut args = args.into_iter();
        let mut path = None;
        let mut integration = false;
        let mut grants = TestGrants::default();
        let mut fixture_path = None;
        let mut backend = None;
        let mut filter = None;
        let mut list = false;
        let mut show_output = false;
        let mut seed = None;
        let mut format = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--integration" => {
                    if integration {
                        return Err("`--integration` may be specified only once".to_string());
                    }
                    integration = true;
                }
                "--dir" => {
                    let root = args
                        .next()
                        .ok_or_else(|| "`--dir` requires a filesystem root".to_string())?;
                    grants.dir_roots.push(std::path::PathBuf::from(root));
                }
                "--net" => {
                    let addr = args
                        .next()
                        .ok_or_else(|| "`--net` requires a host or host:port".to_string())?;
                    grants.net_allow.push(addr);
                }
                "--fixtures" => {
                    let plan = args
                        .next()
                        .ok_or_else(|| "`--fixtures` requires a fixture plan path".to_string())?;
                    if fixture_path
                        .replace(std::path::PathBuf::from(plan))
                        .is_some()
                    {
                        return Err("`--fixtures` may be specified only once".to_string());
                    }
                }
                "--backend" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "`--backend` requires interpreter, wasm, or both".to_string())?;
                    let parsed = match value.as_str() {
                        "interpreter" => TestBackend::Interpreter,
                        "wasm" => TestBackend::Wasm,
                        "both" => TestBackend::Both,
                        _ => {
                            return Err(format!(
                                "unknown test backend `{value}`; expected interpreter, wasm, or both"
                            ));
                        }
                    };
                    if backend.replace(parsed).is_some() {
                        return Err("`--backend` may be specified only once".to_string());
                    }
                }
                "--filter" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "`--filter` requires a non-empty substring".to_string())?;
                    if value.is_empty() {
                        return Err("`--filter` requires a non-empty substring".to_string());
                    }
                    if filter.replace(value).is_some() {
                        return Err("`--filter` may be specified only once".to_string());
                    }
                }
                "--list" => {
                    if list {
                        return Err("`--list` may be specified only once".to_string());
                    }
                    list = true;
                }
                "--show-output" => {
                    if show_output {
                        return Err("`--show-output` may be specified only once".to_string());
                    }
                    show_output = true;
                }
                "--seed" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "`--seed` requires an unsigned 64-bit integer".to_string())?;
                    let parsed = value.parse::<u64>().map_err(|_| {
                        format!("invalid test seed `{value}`; expected an unsigned 64-bit integer")
                    })?;
                    if seed.replace(parsed).is_some() {
                        return Err("`--seed` may be specified only once".to_string());
                    }
                }
                "--format" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "`--format` requires human or json".to_string())?;
                    let parsed = match value.as_str() {
                        "human" => TestOutputFormat::Human,
                        "json" => TestOutputFormat::Json,
                        _ => {
                            return Err(format!(
                                "unknown test output format `{value}`; expected human or json"
                            ));
                        }
                    };
                    if format.replace(parsed).is_some() {
                        return Err("`--format` may be specified only once".to_string());
                    }
                }
                flag if flag.starts_with('-') => {
                    return Err(format!("unknown `witchy test` option `{flag}`"));
                }
                value => {
                    if path.replace(value.to_string()).is_some() {
                        return Err("`witchy test` accepts exactly one file or directory".to_string());
                    }
                }
            }
        }
        let path = path.ok_or_else(|| "`witchy test` requires a file or directory".to_string())?;
        if !integration && (!grants.dir_roots.is_empty() || !grants.net_allow.is_empty()) {
            return Err("real `--dir`/`--net` grants require `witchy test --integration`".to_string());
        }
        if fixture_path.is_some() && integration {
            return Err(
                "`--fixtures` and `--integration` are mutually exclusive; fixture tests receive zero real authority"
                    .to_string(),
            );
        }
        if fixture_path.is_none() && backend.is_some() {
            return Err("`--backend` currently applies to `--fixtures` runs".to_string());
        }
        if fixture_path.is_none() && seed.is_some() {
            return Err("`--seed` requires `--fixtures` with a Rand provider".to_string());
        }
        if list && show_output {
            return Err("`--list` cannot be combined with `--show-output`".to_string());
        }
        let backend = backend.unwrap_or(if fixture_path.is_some() {
            TestBackend::Both
        } else {
            TestBackend::Wasm
        });
        Ok(Self {
            path,
            integration,
            grants,
            fixture_path,
            backend,
            filter,
            list,
            show_output,
            seed,
            format: format.unwrap_or(TestOutputFormat::Human),
        })
    }
}

#[derive(Clone, Copy)]
struct TestRunPolicy<'a> {
    integration: bool,
    real_grants: bool,
    fixture_record_authority: bool,
    grants: &'a TestGrants,
    fixture_plan: Option<&'a FixturePlan>,
    backend: TestBackend,
    filter: Option<&'a str>,
    list: bool,
}

/// Rewrite the placeholder call `witchy_test_target()` in a synthesized test-driver
/// expression to the real (linker-qualified) test name — so the parser never has to
/// re-read `mod.fn` as a method call. The placeholder may sit anywhere in the driver
/// body: bare (`witchy_test_target()`), or as an argument (`task.run(
/// witchy_test_target())`, the async driver), so this recurses through calls,
/// method calls, and unary ops.
fn patch_test_target(expr: &mut ast::Expr, name: &str, args: &[ast::Expr]) {
    match expr {
        ast::Expr::Call {
            name: n,
            args: call_args,
        } => {
            if n == "witchy_test_target" {
                *n = name.to_string();
                *call_args = args.to_vec();
            } else {
                for arg in call_args {
                    patch_test_target(arg, name, args);
                }
            }
        }
        ast::Expr::MethodCall {
            receiver,
            args: call_args,
            ..
        } => {
            patch_test_target(receiver, name, args);
            for arg in call_args {
                patch_test_target(arg, name, args);
            }
        }
        ast::Expr::Unary { expr, .. } => patch_test_target(expr, name, args),
        _ => {}
    }
}

/// The bare names of every `test_*` function in the UNLOWERED source,
/// split into `(async, gen)` sets. Async lowering (`generators` too) runs during
/// `link`, erasing `is_async`/`is_gen` and rewriting the bodies, so the linked module
/// can no longer tell an async or generator test from a plain one — this recovers
/// that shape from the raw parse. A parse/read failure yields empty sets (the linked
/// module still fails to compile and is reported separately).
fn raw_test_shapes(path: &str) -> (std::collections::HashSet<String>, std::collections::HashSet<String>) {
    let mut async_tests = std::collections::HashSet::new();
    let mut gen_tests = std::collections::HashSet::new();
    if let Ok(src) = std::fs::read_to_string(path) {
        if let Ok(module) = parser::parse_module(&src) {
            for it in &module.items {
                if let ast::Item::Function(f) = it {
                    if f.name.starts_with("test_") {
                        if f.is_async {
                            async_tests.insert(f.name.clone());
                        } else if f.is_gen {
                            gen_tests.insert(f.name.clone());
                        }
                    }
                }
            }
        }
    }
    (async_tests, gen_tests)
}

fn validate_integration_test_params(
    linked: &ast::Module,
    test: &str,
    params: &[ast::Param],
    policy: TestRunPolicy<'_>,
) -> Result<(), String> {
    if let Some(plan) = policy.fixture_plan {
        fixture_driver_shape(
            linked,
            test,
            params,
            plan,
            policy.fixture_record_authority,
        )?;
        return Ok(());
    }
    if !policy.integration && !params.is_empty() {
        return Err(format!(
            "test `{test}` declares capability parameter(s); run it with `witchy test --integration` and explicit grants"
        ));
    }
    if !policy.integration {
        return Ok(());
    }

    let mut dir_count = 0usize;
    let mut needs_net = false;
    for param in params {
        let Some(ty) = param.ty.as_ref() else {
            return Err(format!(
                "integration test `{test}` parameter `{}` needs an explicit capability type (`Console`, `Dir`, or `Net`)",
                param.name
            ));
        };
        let ast::Type::Named(name, _) = ty.unqualified() else {
            return Err(format!(
                "integration test `{test}` parameter `{}` must be a `Console`, `Dir`, or `Net` capability",
                param.name
            ));
        };
        match name.as_str() {
            "Console" => {}
            "Dir" => dir_count += 1,
            "Net" => needs_net = true,
            other => {
                return Err(format!(
                    "integration test `{test}` parameter `{}` has unsupported capability type `{other}`; this tier currently accepts `Console`, `Dir`, and `Net`",
                    param.name
                ));
            }
        }
    }

    if !policy.real_grants && (dir_count > 0 || needs_net) {
        return Err(format!(
            "dependency test `{test}` requests real authority, but dependency tests receive zero real grants even under `--integration`"
        ));
    }
    if policy.grants.dir_roots.len() < dir_count {
        return Err(format!(
            "integration test `{test}` requires {dir_count} `Dir` grant(s), but {} were provided; repeat `--dir <root>`",
            policy.grants.dir_roots.len()
        ));
    }
    if needs_net && policy.grants.net_allow.is_empty() {
        return Err(format!(
            "integration test `{test}` requires a `Net` grant; provide at least one `--net <addr>`"
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct FixtureDriverShape {
    params: Vec<ast::Param>,
    args: Vec<ast::Expr>,
}

fn fixture_record_type<'a>(
    module: &'a ast::Module,
    requested: &str,
) -> Result<Option<&'a ast::TypeDef>, String> {
    let exact = module.items.iter().find_map(|item| match item {
        ast::Item::Type(definition) if definition.name == requested => Some(definition),
        _ => None,
    });
    if exact.is_some() {
        return Ok(exact);
    }
    let matches = module
        .items
        .iter()
        .filter_map(|item| match item {
            ast::Item::Type(definition)
                if definition.name.rsplit('.').next() == Some(requested) =>
            {
                Some(definition)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Ok(None),
        [definition] => Ok(Some(*definition)),
        _ => Err(format!(
            "fixture capability record type `{requested}` is ambiguous after linking"
        )),
    }
}

fn fixture_leaf_declared(
    test: &str,
    path: &str,
    ty: &ast::Type,
    plan: &FixturePlan,
) -> Result<bool, String> {
    if typeck::is_args_type(ty) {
        if plan.argv.is_none() {
            return Err(format!(
                "fixture test `{test}` field `{path}` requires argv, but the fixture plan does not declare it"
            ));
        }
        return Ok(true);
    }
    let ast::Type::Named(name, _) = ty.unqualified() else {
        return Ok(false);
    };
    let declared = match name.as_str() {
        "Console" => Some(plan.console.is_some()),
        "Clock" => Some(plan.clock.is_some()),
        "Rand" => Some(plan.rand.is_some()),
        "Env" => Some(plan.env.is_some()),
        "Dir" => Some(plan.filesystem.is_some()),
        "Fetch" => Some(plan.fetch.is_some()),
        "SecretStore" => Some(plan.secrets.is_some()),
        "Exec" => Some(plan.exec.is_some() && plan.filesystem.is_some()),
        "File" => {
            return Err(format!(
                "fixture test `{test}` field `{path}` requests a root `File`; fixture File handles must be derived from a declared `Dir`"
            ));
        }
        "Secret" => {
            return Err(format!(
                "fixture test `{test}` field `{path}` requests a root `Secret`; fixture secrets must be derived from a declared `SecretStore`"
            ));
        }
        "Net" => {
            return Err(format!(
                "fixture test `{test}` field `{path}` requests raw `Net`, which has no deterministic fixture provider"
            ));
        }
        _ => None,
    };
    match declared {
        Some(true) => Ok(true),
        Some(false) => Err(format!(
            "fixture test `{test}` field `{path}` requires `{name}`, but the fixture plan does not declare it"
        )),
        None => Ok(false),
    }
}

struct FixtureShapeBuilder<'a> {
    module: &'a ast::Module,
    plan: &'a FixturePlan,
    test: &'a str,
    params: Vec<ast::Param>,
    next_root: usize,
    visiting: std::collections::HashSet<String>,
}

impl FixtureShapeBuilder<'_> {
    fn value(
        &mut self,
        ty: &ast::Type,
        path: &str,
        top_level: bool,
        record_authority: bool,
    ) -> Result<ast::Expr, String> {
        if fixture_leaf_declared(self.test, path, ty, self.plan)? {
            let name = format!("__fixture_root_{}", self.next_root);
            self.next_root += 1;
            self.params.push(ast::Param {
                name: name.clone(),
                ty: Some(ty.clone()),
                convention: ast::Convention::default(),
                default: None,
            });
            return Ok(ast::Expr::Var(name));
        }
        let ast::Type::Named(name, arguments) = ty.unqualified() else {
            return Err(format!(
                "fixture test `{}` field `{path}` has unsupported fixture type `{}`",
                self.test,
                witchy_syntax::format::type_str(ty)
            ));
        };
        let Some(definition) = fixture_record_type(self.module, name)? else {
            return Err(format!(
                "fixture test `{}` field `{path}` has unsupported fixture type `{name}`",
                self.test
            ));
        };
        if top_level && !definition.is_capability {
            return Err(format!(
                "fixture test `{}` parameter `{path}` is `{name}`, but only a capability record may aggregate fixture roots",
                self.test
            ));
        }
        if top_level && !record_authority {
            return Err(format!(
                "dependency test `{}` cannot receive compiler-assembled fixture capability record `{name}`",
                self.test
            ));
        }
        if !arguments.is_empty()
            || !witchy_syntax::ast::effective_type_def_params(definition).is_empty()
        {
            return Err(format!(
                "fixture test `{}` field `{path}` uses generic capability record `{name}`; fixture record assembly requires a concrete non-generic record",
                self.test
            ));
        }
        let [variant] = definition.variants.as_slice() else {
            return Err(format!(
                "fixture test `{}` field `{path}` uses `{name}`, but fixture aggregation requires one named-field record variant",
                self.test
            ));
        };
        if variant.field_names.len() != variant.fields.len() || variant.fields.is_empty() {
            return Err(format!(
                "fixture test `{}` field `{path}` uses `{name}`, but fixture aggregation requires a non-empty named-field record",
                self.test
            ));
        }
        if !self.visiting.insert(definition.name.clone()) {
            return Err(format!(
                "fixture test `{}` field `{path}` recursively contains capability record `{name}`",
                self.test
            ));
        }
        let mut fields = Vec::with_capacity(variant.fields.len());
        for (field_name, field_type) in variant.field_names.iter().zip(&variant.fields) {
            fields.push(self.value(
                field_type,
                &format!("{path}.{field_name}"),
                false,
                record_authority,
            )?);
        }
        self.visiting.remove(&definition.name);
        Ok(ast::Expr::Ctor {
            name: variant.name.clone(),
            args: fields,
        })
    }
}

fn fixture_driver_shape(
    module: &ast::Module,
    test: &str,
    params: &[ast::Param],
    plan: &FixturePlan,
    record_authority: bool,
) -> Result<FixtureDriverShape, String> {
    let mut builder = FixtureShapeBuilder {
        module,
        plan,
        test,
        params: Vec::new(),
        next_root: 0,
        visiting: std::collections::HashSet::new(),
    };
    let mut args = Vec::with_capacity(params.len());
    for param in params {
        let Some(ty) = param.ty.as_ref() else {
            return Err(format!(
                "fixture test `{test}` parameter `{}` needs an explicit capability type",
                param.name
            ));
        };
        args.push(builder.value(ty, &param.name, true, record_authority)?);
    }
    Ok(FixtureDriverShape {
        params: builder.params,
        args,
    })
}

struct FixtureBackendOutcome {
    passed: bool,
    output: Vec<String>,
    error: Option<String>,
    transcript: TestTranscript,
}

fn run_interpreter_fixtures(
    checked: &witchy_types::pipeline::CheckedModule,
    plan: &FixturePlan,
) -> Result<FixtureBackendOutcome, String> {
    let outcome = interpreter::run_checked_module_fixtures(checked, plan.clone())
        .map_err(|error| error.to_string())?;
    let (passed, output, error) = match outcome.result {
        interpreter::FixtureProgramResult::Passed { output, .. } => (true, output, None),
        interpreter::FixtureProgramResult::Failed { output, error } => {
            (false, output, Some(error.to_string()))
        }
    };
    Ok(FixtureBackendOutcome {
        passed,
        output,
        error,
        transcript: outcome.transcript,
    })
}

fn run_wasm_fixtures(
    checked: &witchy_types::pipeline::CheckedModule,
    plan: &FixturePlan,
) -> Result<FixtureBackendOutcome, String> {
    let bytes = match codegen::compile_checked_module_binary(checked) {
        codegen::LoweringOutcome::Lowered(bytes) => bytes,
        codegen::LoweringOutcome::Unsupported(reason) => return Err(reason.to_string()),
        codegen::LoweringOutcome::Rejected(error) => return Err(error.to_string()),
    };
    let mut runtime = runtime::Runtime::batch().map_err(|error| error.to_string())?;
    let outcome = runtime
        .run_fixtures(&bytes, plan.clone(), crate::RUN_MEMORY_PAGES)
        .map_err(|error| error.to_string())?;
    let (passed, error) = match outcome.result {
        runtime::FixtureWasmResult::Passed => (true, None),
        runtime::FixtureWasmResult::Failed { error } => (false, Some(error)),
    };
    Ok(FixtureBackendOutcome {
        passed,
        output: outcome.output,
        error,
        transcript: outcome.transcript,
    })
}

fn same_fixture_evidence(left: &TestTranscript, right: &TestTranscript) -> bool {
    let same_result_kind = matches!(
        (&left.result, &right.result),
        (TestResult::Passed, TestResult::Passed)
            | (TestResult::Failed { .. }, TestResult::Failed { .. })
            | (
                TestResult::InfrastructureError { .. },
                TestResult::InfrastructureError { .. }
            )
    );
    left.version == right.version
        && left.seed == right.seed
        && left.events == right.events
        && left.stdout == right.stdout
        && left.stderr == right.stderr
        && same_result_kind
}

fn run_fixture_test(
    checked: &witchy_types::pipeline::CheckedModule,
    plan: &FixturePlan,
    backend: TestBackend,
) -> Result<FixtureBackendOutcome, String> {
    match backend {
        TestBackend::Interpreter => run_interpreter_fixtures(checked, plan),
        TestBackend::Wasm => run_wasm_fixtures(checked, plan),
        TestBackend::Both => {
            let interpreted = run_interpreter_fixtures(checked, plan)?;
            let compiled = run_wasm_fixtures(checked, plan)?;
            if interpreted.passed != compiled.passed
                || interpreted.output != compiled.output
                || !same_fixture_evidence(&interpreted.transcript, &compiled.transcript)
            {
                return Err(format!(
                    "fixture backend divergence\ninterpreter: output={:?}, transcript={:?}\nwasm: output={:?}, transcript={:?}",
                    interpreted.output,
                    interpreted.transcript,
                    compiled.output,
                    compiled.transcript
                ));
            }
            Ok(compiled)
        }
    }
}

#[derive(Default)]
struct TestModuleResult {
    passed: Vec<String>,
    failed: Vec<TestFailure>,
    output: std::collections::BTreeMap<String, Vec<String>>,
    transcripts: std::collections::BTreeMap<String, TestTranscript>,
}

/// Discover and run the tests in an already-linked module (`stem` = the entry file's
/// stem). Every function named `test_*` that the ENTRY file itself declares is
/// invoked through a synthesized `main` on compiled WASM. Plain tests take no
/// parameters and receive no real authority; integration tests forward only their
/// declared capability parameters under the caller's explicit grant policy.
/// `async_tests`/`gen_tests` are the bare names of the entry file's async/gen tests
/// (from `raw_test_shapes`, since lowering erased the AST flags). Returns
/// `(passed, failures)` where each failure is `(name, message)`.
fn run_tests_in_module(
    checked: &witchy_types::pipeline::CheckedModule,
    stem: &str,
    async_tests: &std::collections::HashSet<String>,
    gen_tests: &std::collections::HashSet<String>,
    policy: TestRunPolicy<'_>,
) -> Result<TestModuleResult, String> {
    let linked = checked.module();
    // BUG-177: a test run honors `mode opt` like `check`/`run` — a copy-cliff or a
    // missing ownership convention fails the run, it is not silently ignored.
    enforce_performance_modes(linked, stem)?;
    // Post-link names are module-qualified (`suite.test_x`); match on the bare name.
    // BUG-185: run only the ENTRY file's OWN tests. Linking pulls an imported
    // module's `test_*` functions into `linked` too (as `othermod.test_x`); running
    // them here would DOUBLE-count them — they run again when that module's own file
    // is swept. `is_entry_function` keeps just `main` + the `{stem}.`-prefixed items.
    let tests: Vec<(String, bool, bool, Vec<ast::Param>)> = linked
        .items
        .iter()
        .filter_map(|it| match it {
            ast::Item::Function(f)
                if is_entry_function(&f.name, stem)
                    && f.name.rsplit('.').next().unwrap_or(&f.name).starts_with("test_") =>
            {
                let bare = f.name.rsplit('.').next().unwrap_or(&f.name);
                Some((
                    f.name.clone(),
                    async_tests.contains(bare),
                    gen_tests.contains(bare),
                    f.params.clone(),
                ))
            }
            _ => None,
        })
        .collect();
    let mut result = TestModuleResult::default();
    for (test, is_async, is_gen, params) in tests {
        if policy
            .filter
            .is_some_and(|filter| !test.contains(filter))
        {
            continue;
        }
        if policy.list {
            result.passed.push(test);
            continue;
        }
        // BUG-184: an async/gen test's body does NOT run when the function is merely
        // CALLED — calling an `async fn` yields a `Task` and a `gen fn` yields an
        // iterator, both discarded, so a `fail_with` inside never fires and the test
        // FALSELY passes. An `async fn test_*()` is already lowered (when the file was
        // linked) to a `Task(Nil)`-returning function, so DRIVE it to completion with
        // `task.run` — which surfaces the abort. A `gen fn` yields a sequence rather
        // than running to completion, so it cannot be a test; report it as a failure
        // rather than a silent pass.
        if is_gen {
            result.failed.push((
                test,
                "a `gen fn` cannot be run as a test — it yields a sequence instead of running to completion".to_string(),
            ));
            continue;
        }
        if let Err(e) = validate_integration_test_params(linked, &test, &params, policy) {
            result.failed.push((test, e));
            continue;
        }
        let driver_shape = if let Some(plan) = policy.fixture_plan {
            match fixture_driver_shape(
                linked,
                &test,
                &params,
                plan,
                policy.fixture_record_authority,
            ) {
                Ok(shape) => shape,
                Err(error) => {
                    result.failed.push((test, error));
                    continue;
                }
            }
        } else {
            FixtureDriverShape {
                params: params.clone(),
                args: params
                    .iter()
                    .map(|param| ast::Expr::Var(param.name.clone()))
                    .collect(),
            }
        };
        // Synthesize a `main` (replacing any real one) that runs the test, and run it.
        // The test name is linker-qualified (`suite.test_x`), which the parser would
        // read as a method call — so parse a placeholder and patch the call in the AST.
        // `task.run` is in scope: async lowering imported `task` into a file with any
        // `async fn`, which is exactly the case an async test needs it.
        let mut m = linked.clone();
        m.items
            .retain(|it| !matches!(it, ast::Item::Function(f) if f.name == "main"));
        let driver_src = if is_async {
            "fn main():\n    task.run(witchy_test_target())\n"
        } else {
            "fn main():\n    witchy_test_target()\n"
        };
        let mut driver = parser::parse_module(driver_src).map_err(|e| e.to_string())?;
        for it in &mut driver.items {
            if let ast::Item::Function(f) = it {
                f.params = driver_shape.params.clone();
                if let Some(ast::Stmt::Expr(e)) = f.body.stmts.first_mut() {
                    patch_test_target(e, &test, &driver_shape.args);
                }
            }
        }
        m.items.extend(driver.items);
        let checked = witchy_types::pipeline::check_synthetic_module(m)
            .map_err(|error| error.to_string())?;
        // Run the test on the COMPILED WASM tier — the tier users ship — not the
        // interpreter oracle: a `witchy test` that passes must reflect the backend
        // that actually runs in production. A `testing.assert` / `fail_with` lowers
        // to `__witchy_abort`, which is authority-free and always linked by the
        // runtime. Plain tests run under zero real host capability grants;
        // integration tests use the same explicit runtime-grant path as sandbox/run.
        // The synthesized `main` plus codegen's reachability pruning keep unused
        // effectful production functions out of the test artifact. A module that
        // does not lower is itself a failure: the test cannot run where it ships.
        let (outcome, transcript) = if let Some(plan) = policy.fixture_plan {
            match run_fixture_test(&checked, plan, policy.backend) {
                Ok(fixture) => {
                    let FixtureBackendOutcome {
                        passed,
                        output,
                        error,
                        transcript,
                    } = fixture;
                    let outcome = if passed {
                        Ok(output)
                    } else {
                        Err((
                            error.unwrap_or_else(|| {
                                "fixture test failed without a diagnostic".to_string()
                            }),
                            output,
                        ))
                    };
                    (outcome, Some(transcript))
                }
                Err(error) => (Err((error, Vec::new())), None),
            }
        } else if policy.integration {
            (
                run_checked_compiled(
                    &checked,
                    policy.grants.dir_roots.clone(),
                    Vec::new(),
                    policy.grants.net_allow.clone(),
                    Vec::new(),
                    None,
                    None,
                    Vec::new(),
                    Vec::new(),
                    None,
                    Vec::new(),
                    Vec::new(),
                    true,
                    witchy_confinement::EnforcementMode::Disabled,
                )
                .map(|(output, _)| output)
                .map_err(|error| (error, Vec::new())),
                None,
            )
        } else {
            (
                match codegen::compile_checked_module_binary(&checked) {
                    codegen::LoweringOutcome::Lowered(bytes) => {
                        run_wasm_test_bytes(&bytes).map_err(|error| (error, Vec::new()))
                    }
                    codegen::LoweringOutcome::Unsupported(reason) => {
                        Err((reason.to_string(), Vec::new()))
                    }
                    codegen::LoweringOutcome::Rejected(error) => {
                        Err((error.to_string(), Vec::new()))
                    }
                },
                None,
            )
        };
        match outcome {
            Ok(output) => {
                result.output.insert(test.clone(), output);
                if let Some(transcript) = transcript {
                    result.transcripts.insert(test.clone(), transcript);
                }
                result.passed.push(test);
            }
            Err((msg, output)) => {
                if !output.is_empty() {
                    result.output.insert(test.clone(), output);
                }
                if let Some(transcript) = transcript {
                    result.transcripts.insert(test.clone(), transcript);
                }
                result.failed.push((test, msg));
            }
        }
    }
    Ok(result)
}

/// Link `path` and run its own tests — the single-file convenience the test suite
/// drives. Mirrors what `run_tests` does per file (link, recover async/gen shapes,
/// dispatch to `run_tests_in_module`).
#[cfg(test)]
pub(crate) fn run_tests_in_file(path: &str) -> Result<(Vec<String>, Vec<TestFailure>), String> {
    let (linked, stem) = link_test_file(path)?;
    let (async_tests, gen_tests) = raw_test_shapes(path);
    let grants = TestGrants::default();
    let result = run_tests_in_module(
        &linked,
        &stem,
        &async_tests,
        &gen_tests,
        TestRunPolicy {
            integration: false,
            real_grants: false,
            fixture_record_authority: true,
            grants: &grants,
            fixture_plan: None,
            backend: TestBackend::Wasm,
            filter: None,
            list: false,
        },
    )?;
    Ok((result.passed, result.failed))
}

#[cfg(test)]
mod test_mode_link_tests {
    use super::{
        raw_test_shapes, run_tests_in_file, run_tests_in_module, TestBackend, TestGrants,
        TestOptions, TestRunPolicy,
    };
    use crate::link_file;
    use witchy_testkit::{ConsoleFixture, FixturePlan};

    fn unique_dir(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("witchy_{name}_{}_{}", std::process::id(), nanos))
    }

    #[test]
    fn witchy_test_allows_entry_to_construct_foreign_sealed_data() {
        let dir = unique_dir("sealed_test_mode");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("sealed_lib.witchy"),
            "sealed type Version:\n    Version(Int, Int, Int)\n\n\
             pub fn major(v: Version) -> Int:\n    \
             match v:\n        Version(n, _, _) -> n\n",
        )
        .unwrap();
        let suite = dir.join("suite.witchy");
        std::fs::write(
            &suite,
            "import sealed_lib\nimport testing\n\n\
             fn test_constructs_domain_edge_case():\n    \
             let v = sealed_lib.Version(99, 0, 0)\n    \
             testing.assert_int_eq(sealed_lib.major(v), 99)\n",
        )
        .unwrap();

        let prod = link_file(suite.to_str().unwrap()).expect_err("production link must reject");
        assert!(prod.contains("sealed type") && prod.contains("Version"), "{prod}");

        let (passed, failed) = run_tests_in_file(suite.to_str().unwrap()).expect("test mode links");
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(passed, vec!["suite.test_constructs_domain_edge_case".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn witchy_test_prunes_unused_effectful_main_under_zero_grant() {
        let dir = unique_dir("zero_grant_prunes_unused_effects");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let suite = dir.join("suite.witchy");
        std::fs::write(
            &suite,
            "import testing\n\n\
             fn main(console: Console, root: Dir[Read]):\n    \
             console.print(root.read(\"secret.txt\"))\n\n\
             fn test_pure_logic():\n    \
             testing.assert_int_eq(2 + 2, 4)\n",
        )
        .unwrap();

        let (passed, failed) = run_tests_in_file(suite.to_str().unwrap())
            .expect("unused effectful main is replaced by the test driver");
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(passed, vec!["suite.test_pure_logic".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn witchy_test_zero_grant_keeps_abort_diagnostics() {
        let dir = unique_dir("zero_grant_abort");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let suite = dir.join("suite.witchy");
        std::fs::write(
            &suite,
            "import testing\n\n\
             fn test_failure_message():\n    testing.fail_with(\"boom\")\n",
        )
        .unwrap();

        let (passed, failed) = run_tests_in_file(suite.to_str().unwrap())
            .expect("abort host import is authority-free under zero grant");
        assert!(passed.is_empty(), "{passed:?}");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, "suite.test_failure_message");
        assert!(failed[0].1.contains("boom"), "{failed:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fixture_options_default_to_backend_parity_and_reject_real_grants() {
        let options = TestOptions::parse([
            "--fixtures".to_string(),
            "plan.json".to_string(),
            "suite.witchy".to_string(),
        ])
        .expect("fixture options");
        assert_eq!(options.backend, TestBackend::Both);
        assert_eq!(
            options.fixture_path.as_deref(),
            Some(std::path::Path::new("plan.json"))
        );
        let error = TestOptions::parse([
            "--fixtures".to_string(),
            "plan.json".to_string(),
            "--integration".to_string(),
            "suite.witchy".to_string(),
        ])
        .expect_err("fixtures cannot inherit integration grants");
        assert!(error.contains("zero real authority"), "{error}");
    }

    #[test]
    fn fixture_test_runs_real_witchy_source_with_identical_backend_evidence() {
        let dir = unique_dir("fixture_backend_parity");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let suite = dir.join("suite.witchy");
        std::fs::write(
            &suite,
            "fn test_console(console: Console):\n    console.print(\"fixture\")\n",
        )
        .unwrap();
        let path = suite.to_str().unwrap();
        let (linked, stem) = crate::link_test_file(path).expect("test link");
        let (async_tests, gen_tests) = raw_test_shapes(path);
        let grants = TestGrants::default();
        let plan = FixturePlan {
            console: Some(ConsoleFixture::default()),
            ..FixturePlan::default()
        };
        let result = run_tests_in_module(
            &linked,
            &stem,
            &async_tests,
            &gen_tests,
            TestRunPolicy {
                integration: false,
                real_grants: false,
                fixture_record_authority: true,
                grants: &grants,
                fixture_plan: Some(&plan),
                backend: TestBackend::Both,
                filter: None,
                list: false,
            },
        )
        .expect("fixture parity run");
        assert!(result.failed.is_empty(), "{:?}", result.failed);
        assert_eq!(result.passed, vec!["suite.test_console".to_string()]);
        let transcript = result
            .transcripts
            .get("suite.test_console")
            .expect("fixture transcript retained at the CLI result boundary");
        assert_eq!(transcript.stdout, vec!["fixture".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fixture_test_assembles_owned_capability_record_on_both_backends() {
        let dir = unique_dir("fixture_capability_record");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let suite = dir.join("suite.witchy");
        std::fs::write(
            &suite,
            "capability TestRoot:\n    console: Console\n    args: List(String)\n\n\
             fn test_record(root: TestRoot):\n    \
             match root:\n        \
             TestRoot(console, args) -> console.print(args.at(0))\n",
        )
        .unwrap();
        let path = suite.to_str().unwrap();
        let (linked, stem) = crate::link_test_file(path).expect("test link");
        let (async_tests, gen_tests) = raw_test_shapes(path);
        let grants = TestGrants::default();
        let plan = FixturePlan {
            console: Some(ConsoleFixture::default()),
            argv: Some(vec!["record-root".to_string()]),
            ..FixturePlan::default()
        };
        let result = run_tests_in_module(
            &linked,
            &stem,
            &async_tests,
            &gen_tests,
            TestRunPolicy {
                integration: false,
                real_grants: false,
                fixture_record_authority: true,
                grants: &grants,
                fixture_plan: Some(&plan),
                backend: TestBackend::Both,
                filter: None,
                list: false,
            },
        )
        .expect("owned fixture capability record run");
        assert!(result.failed.is_empty(), "{:?}", result.failed);
        assert_eq!(
            result.output.get("suite.test_record"),
            Some(&vec!["record-root".to_string()])
        );

        let denied = run_tests_in_module(
            &linked,
            &stem,
            &async_tests,
            &gen_tests,
            TestRunPolicy {
                integration: false,
                real_grants: false,
                fixture_record_authority: false,
                grants: &grants,
                fixture_plan: Some(&plan),
                backend: TestBackend::Both,
                filter: None,
                list: false,
            },
        )
        .expect("dependency denial is an ordinary test failure");
        assert!(denied.passed.is_empty(), "{:?}", denied.passed);
        assert_eq!(denied.failed.len(), 1);
        assert!(
            denied.failed[0]
                .1
                .contains("dependency test")
                && denied.failed[0]
                    .1
                    .contains("compiler-assembled fixture capability record"),
            "{:?}",
            denied.failed
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fixture_capability_record_recursion_is_rejected_before_execution() {
        let module = crate::parser::parse_module(
            "capability RecursiveRoot:\n    next: RecursiveRoot\n\n\
             fn test_recursive(root: RecursiveRoot):\n    nil\n",
        )
        .expect("recursive capability record parses");
        let function = module
            .items
            .iter()
            .find_map(|item| match item {
                crate::ast::Item::Function(function)
                    if function.name == "test_recursive" =>
                {
                    Some(function)
                }
                _ => None,
            })
            .expect("test function");
        let error = super::fixture_driver_shape(
            &module,
            "test_recursive",
            &function.params,
            &FixturePlan::default(),
            true,
        )
        .expect_err("recursive fixture aggregate must terminate with a diagnostic");
        assert!(
            error.contains("recursively contains capability record `RecursiveRoot`"),
            "{error}"
        );
    }
}

#[derive(Debug, Default)]
struct TestPackageOwnership {
    dependency_roots: Vec<std::path::PathBuf>,
}

impl TestPackageOwnership {
    fn resolve(target: &str) -> Result<Self, String> {
        let target = std::fs::canonicalize(target)
            .map_err(|e| format!("cannot resolve test target `{target}`: {e}"))?;
        let start = if target.is_dir() {
            target.as_path()
        } else {
            target.parent().unwrap_or_else(|| std::path::Path::new("."))
        };
        let Some(package_root) = package_root_for(start) else {
            return Ok(Self::default());
        };
        let mut roots = std::collections::BTreeSet::new();
        let mut visited = std::collections::HashSet::new();
        collect_resolved_dependency_roots(&package_root, &mut roots, &mut visited)?;
        Ok(Self { dependency_roots: roots.into_iter().collect() })
    }

    fn owns(&self, file: &str) -> Result<bool, String> {
        let file = std::fs::canonicalize(file)
            .map_err(|e| format!("cannot resolve test file `{file}`: {e}"))?;
        Ok(!self.dependency_roots.iter().any(|root| file.starts_with(root)))
    }
}

fn package_root_for(start: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut at = Some(start);
    while let Some(dir) = at {
        if dir.join("witchy.toml").is_file() {
            return std::fs::canonicalize(dir).ok();
        }
        at = dir.parent();
    }
    None
}

fn dependency_alias(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

fn locked_registry_aliases(package_root: &std::path::Path) -> Result<std::collections::HashSet<String>, String> {
    let path = package_root.join("witchy.lock");
    if !path.is_file() {
        return Ok(std::collections::HashSet::new());
    }
    let source = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
    let lock: toml::Value = toml::from_str(&source)
        .map_err(|e| format!("cannot parse `{}`: {e}", path.display()))?;
    let mut aliases = std::collections::HashSet::new();
    for entry in lock.get("rune").and_then(toml::Value::as_array).into_iter().flatten() {
        if entry.get("source").and_then(toml::Value::as_str) != Some("coven") {
            continue;
        }
        let alias = entry
            .get("alias")
            .and_then(toml::Value::as_str)
            .or_else(|| entry.get("name").and_then(toml::Value::as_str).map(dependency_alias));
        if let Some(alias) = alias {
            aliases.insert(alias.to_string());
        }
    }
    Ok(aliases)
}

fn resolved_dependency_dirs(package_root: &std::path::Path) -> Result<Vec<std::path::PathBuf>, String> {
    let manifest_path = package_root.join("witchy.toml");
    let source = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("cannot read `{}`: {e}", manifest_path.display()))?;
    let manifest: toml::Value = toml::from_str(&source)
        .map_err(|e| format!("cannot parse `{}`: {e}", manifest_path.display()))?;
    let Some(dependencies) = manifest.get("dependencies").and_then(toml::Value::as_table) else {
        return Ok(Vec::new());
    };
    let registry_aliases = locked_registry_aliases(package_root)?;
    let mut dirs = Vec::new();
    for (name, declaration) in dependencies {
        let resolved = declaration
            .as_table()
            .and_then(|inline| inline.get("path"))
            .and_then(toml::Value::as_str)
            .map(|path| package_root.join(path))
            .or_else(|| {
                let alias = dependency_alias(name);
                registry_aliases
                    .contains(alias)
                    .then(|| package_root.join("vendor").join(alias))
            });
        if let Some(dir) = resolved
            && dir.is_dir()
        {
            dirs.push(
                std::fs::canonicalize(&dir)
                    .map_err(|e| format!("cannot resolve dependency `{}`: {e}", dir.display()))?,
            );
        }
    }
    Ok(dirs)
}

fn collect_resolved_dependency_roots(
    package_root: &std::path::Path,
    roots: &mut std::collections::BTreeSet<std::path::PathBuf>,
    visited: &mut std::collections::HashSet<std::path::PathBuf>,
) -> Result<(), String> {
    let package_root = std::fs::canonicalize(package_root)
        .map_err(|e| format!("cannot resolve package `{}`: {e}", package_root.display()))?;
    if !visited.insert(package_root.clone()) {
        return Ok(());
    }
    for dependency in resolved_dependency_dirs(&package_root)? {
        roots.insert(dependency.clone());
        collect_resolved_dependency_roots(&dependency, roots, visited)?;
    }
    Ok(())
}

/// `witchy test <file|dir>`: run in-language tests, print a cargo-style
/// report, and return whether everything passed.
pub(crate) fn run_tests(options: &TestOptions) -> Result<bool, String> {
    let path = &options.path;
    let mut fixture_plan = options
        .fixture_path
        .as_ref()
        .map(|fixture_path| {
            let bytes = std::fs::read(fixture_path).map_err(|error| {
                format!(
                    "cannot read fixture plan `{}`: {error}",
                    fixture_path.display()
                )
            })?;
            witchy_testkit::parse_fixture_plan(&bytes).map_err(|error| {
                format!(
                    "invalid fixture plan `{}`: {error}",
                    fixture_path.display()
                )
            })
        })
        .transpose()?;
    if let Some(seed) = options.seed {
        let plan = fixture_plan
            .as_mut()
            .expect("option parsing requires a fixture plan for --seed");
        let rand = plan
            .rand
            .as_mut()
            .ok_or_else(|| "`--seed` requires the fixture plan to declare `rand`".to_string())?;
        rand.seed = Some(witchy_testkit::U64Text::new(seed));
        plan.validate()
            .map_err(|error| format!("fixture plan is invalid after applying --seed: {error}"))?;
    }
    let mut files: Vec<String> = Vec::new();
    let meta = std::fs::metadata(path).map_err(|e| format!("cannot read `{path}`: {e}"))?;
    let ownership = TestPackageOwnership::resolve(path)?;
    if meta.is_dir() {
        // Collect every `.witchy` under the directory recursively, so a rune's
        // `src/` modules (and the nested runes of a multi-rune project) are all
        // discovered — `witchy test <rune-dir>` runs the whole package's tests.
        fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
            let mut entries: Vec<_> =
                std::fs::read_dir(dir)?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            entries.sort();
            for p in entries {
                if p.is_dir() {
                    collect(&p, out)?;
                } else if p.extension().and_then(|s| s.to_str()) == Some("witchy") {
                    out.push(p);
                }
            }
            Ok(())
        }
        let mut paths = Vec::new();
        collect(std::path::Path::new(path), &mut paths)
            .map_err(|e| format!("cannot read `{path}`: {e}"))?;
        files.extend(paths.into_iter().filter_map(|p| p.to_str().map(String::from)));
    } else {
        files.push(path.to_string());
    }
    let mut total_pass = 0usize;
    let mut total_fail = 0usize;
    let mut json_tests = Vec::new();
    for file in &files {
        // Distinguish a LINK failure from a post-link (compile) failure (BUG-120).
        // In a directory sweep, a file that can't LINK standalone — a module of a
        // multi-rune project that imports a sibling rune via a path dependency, which
        // resolves no local `<import>.witchy` — is skipped, not fatal. But a file
        // that links yet fails to TYPE-CHECK (or violates `mode opt`) is a genuinely
        // BROKEN test file: it must FAIL the run, never be silently skipped as
        // "ok. 0 passed". An explicit single file surfaces even a link error.
        let (linked, stem) = match link_test_file(file) {
            Ok(v) => v,
            Err(e) if meta.is_dir() => {
                if options.format == TestOutputFormat::Json {
                    json_tests.push(serde_json::json!({
                        "file": file,
                        "name": serde_json::Value::Null,
                        "status": "skipped",
                        "error": e,
                        "output": [],
                        "transcript": serde_json::Value::Null,
                    }));
                } else {
                    eprintln!("  skipped {file}: {e}");
                }
                continue;
            }
            Err(e) => return Err(e),
        };
        let (async_tests, gen_tests) = raw_test_shapes(file);
        let owns_test = ownership.owns(file)?;
        let no_real_grants = TestGrants::default();
        let grants = if owns_test { &options.grants } else { &no_real_grants };
        let policy = TestRunPolicy {
            integration: options.integration,
            real_grants: options.integration && owns_test,
            fixture_record_authority: owns_test,
            grants,
            fixture_plan: fixture_plan.as_ref(),
            backend: options.backend,
            filter: options.filter.as_deref(),
            list: options.list,
        };
        let result = match run_tests_in_module(
            &linked,
            &stem,
            &async_tests,
            &gen_tests,
            policy,
        ) {
            Ok(r) => r,
            Err(e) => {
                // Linked OK but broken (a type error or a `mode opt` violation): count
                // it as a failure so the run exits non-zero (BUG-120).
                if options.format == TestOutputFormat::Json {
                    json_tests.push(serde_json::json!({
                        "file": file,
                        "name": serde_json::Value::Null,
                        "status": "failed",
                        "error": e,
                        "output": [],
                        "transcript": serde_json::Value::Null,
                    }));
                } else {
                    println!("running test(s) in {file}");
                    println!("test {file} ... FAILED to compile: {e}");
                }
                total_fail += 1;
                continue;
            }
        };
        if result.passed.is_empty() && result.failed.is_empty() {
            continue;
        }
        if options.format == TestOutputFormat::Human && !options.list {
            println!(
                "running {} test(s) in {file}",
                result.passed.len() + result.failed.len()
            );
        }
        for name in &result.passed {
            let output = result.output.get(name).cloned().unwrap_or_default();
            if options.format == TestOutputFormat::Json {
                json_tests.push(serde_json::json!({
                    "file": file,
                    "name": name,
                    "status": if options.list { "listed" } else { "passed" },
                    "error": serde_json::Value::Null,
                    "output": output,
                    "transcript": result.transcripts.get(name),
                }));
            } else if options.list {
                println!("{name}");
            } else {
                println!("test {name} ... ok");
                if options.show_output {
                    for line in output {
                        println!("  {line}");
                    }
                }
            }
        }
        for (name, msg) in &result.failed {
            if options.format == TestOutputFormat::Json {
                json_tests.push(serde_json::json!({
                    "file": file,
                    "name": name,
                    "status": "failed",
                    "error": msg,
                    "output": result.output.get(name).cloned().unwrap_or_default(),
                    "transcript": result.transcripts.get(name),
                }));
            } else {
                println!("test {name} ... FAILED: {msg}");
                for line in result.output.get(name).into_iter().flatten() {
                    println!("  {line}");
                }
            }
        }
        total_pass += result.passed.len();
        total_fail += result.failed.len();
    }
    if options.format == TestOutputFormat::Json {
        let document = serde_json::json!({
            "schema": 2,
            "tests": json_tests,
            "summary": {
                "status": if total_fail == 0 { "passed" } else { "failed" },
                "passed": total_pass,
                "failed": total_fail,
            },
        });
        println!(
            "{}",
            serde_json::to_string(&document)
                .map_err(|error| format!("cannot serialize test result: {error}"))?
        );
    } else if options.list {
        println!("\n{total_pass} test(s)");
    } else {
        println!(
            "\ntest result: {}. {total_pass} passed; {total_fail} failed",
            if total_fail == 0 { "ok" } else { "FAILED" }
        );
    }
    Ok(total_fail == 0)
}
