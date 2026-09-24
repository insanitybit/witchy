//! RFC-0135/RFC-0165 execution oracle: every admitted program runs through
//! the executable SafeWIR interpreter (the checker's semantics) and through
//! the admitted Wasm; both must match explicit fixture expectations,
//! and neither may end with a live reference-counted object. The oracle catches an
//! encoder defect the plan mirror faithfully reproduces (the destructor
//! scratch-slot leak was one).

use std::time::{Duration, Instant};

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterConfig, InterpreterValue};
use witchy_wir::canonical_safewir::{FunctionId, ValidatedSafeWir};

#[path = "rfc0165_exec.rs"]
mod exec;

#[test]
fn admitted_observer_is_not_called_on_refusal() {
    // An open library parameter has no authenticated closed Dynamic payload
    // catalog. Unlike a newly supported entry capability, this is an explicit
    // missing-authority refusal rather than a coverage-dependent placeholder.
    let unsupported = witchy::resolve_std_only_checked(
        "import dynamic\nfn forward(value: dynamic.Dynamic) -> Result(dynamic.Dynamic, dynamic.DynamicError):\n    dynamic.field(value, \"value\")\n",
    ).expect("open Dynamic library source checks without inventing payload authority");
    let ast = witchy_syntax::parser::parse_module("fn main() -> Int:\n    1\n")
        .expect("AST-only source parses");
    let ast_only = witchy_types::pipeline::link_checked(
        vec![("main".to_owned(), ast)], "main", |_, _, _| Ok(Default::default()),
    ).expect("AST-only module passes real source checking");
    assert!(ast_only.canonical_source().is_none(), "AST checking cannot mint retained source authority");
    for (checked, unsupported) in [(unsupported, true), (ast_only, false)] {
        let mut calls = 0;
        let outcome = codegen::compile_checked_module_artifact_with_observer(
            &checked, |_, _| { calls += 1; },
        );
        assert_eq!(calls, 0, "no observer before successful admission");
        match (outcome, unsupported) {
            (codegen::LoweringOutcome::Unsupported(_), true)
            | (codegen::LoweringOutcome::Rejected(_), false) => {}
            _ => panic!("refusal classification must not change"),
        }
    }
}

#[test]
fn admitted_observer_examples_three_legs() {
    for (label, source) in [
        ("hello", include_str!("../../examples/hello/src/hello.witchy")),
        ("wrap", include_str!("../../examples/wrap/src/wrap.witchy")),
        ("jq", include_str!("../../examples/jq/src/jq.witchy")),
    ] {
        oracle(source, label);
    }
}

#[test]
fn tail_fail_keeps_trap_semantics_three_legs() {
    let template = r#"
fn check_header(valid: Bool):
    if !valid:
        fail("rejected header")

fn main(console: Console):
    console.print("before")
    check_header(FLAG)
    console.print("after")
"#;
    for (flag, traps) in [("true", false), ("false", true)] {
        let source = template.replace("FLAG", flag);
        let checked = witchy::resolve_std_only_checked(&source).expect("tail fail source checks");
        let expected = ["before", "after"].map(str::to_owned).to_vec();
        let artifact = codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("a tail trap is a complete block terminator");
        let entry = artifact.entry().unwrap();
        let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let validated = codegen::validate_checked_module_for_oracle(&checked).expect("oracle validation");
        let capabilities = entry.capabilities().iter().copied()
            .map(InterpreterValue::entry_capability).collect();
        let oracle_run = witchy_wir::canonical_interpreter::interpret_program(
            &validated, function, capabilities, Default::default(),
        );
        if traps {
            assert_eq!(oracle_run.unwrap_err(),
                witchy_wir::canonical_interpreter::InterpretError::DiagnosticAbort {
                    message: "runtime error: `main.check_header`, line 4: rejected header".into(),
                    output: vec!["before".into()],
                });
        } else {
            let run = oracle_run.expect("oracle execution");
            assert_eq!(run.output, expected);
            assert_eq!(run.leaked_objects, 0);
        }
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
            print: true, quiet: true, ..Default::default()
        }, 256).expect("admitted Wasm");
        let result = vm.run();
        if traps {
            let error = result.expect_err("compiled fail must abort");
            assert_eq!(error.root_cause().to_string(), "runtime error: `main.check_header`, line 4: rejected header");
            assert_eq!(vm.output(), ["before"]);
        } else {
            result.expect("Wasm execution");
            assert_eq!(vm.output(), expected);
            assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
        }
    }
}

#[test]
fn dynamic_library_forwarding_does_not_mint_closed_payload_authority() {
    let source = "import dynamic\nfn forward(value: dynamic.Dynamic) -> Result(dynamic.Dynamic, dynamic.DynamicError):\n    dynamic.field(value, \"value\")\n";
    let checked = witchy::resolve_std_only_checked(source).expect("open library signature type-checks");
    assert!(checked.canonical_source().unwrap().typed_module().module().items.iter().all(|item| {
        !matches!(item, witchy::ast::Item::Type(definition) if definition.name == "$canonical.DynamicPayload")
    }));
    codegen::compile_checked_module_artifact(&checked)
        .expect_unsupported("an open existential is not closed payload authority");
}

fn oracle(source: &str, label: &str) -> Vec<String> {
    let checked = witchy::resolve_std_only_checked(source).unwrap_or_else(|e| panic!("{label}: {e}"));
    let (_, output) = codegen::compile_checked_module_artifact_with_observer(
        &checked, |validated, artifact| oracle_validated(validated, artifact, label),
    )
        .expect_lowered(&format!("{label}: the foundation subset admits this program"));
    output
}

fn oracle_validated(
    validated: &ValidatedSafeWir,
    artifact: &witchy_wir::AdmittedCanonicalArtifact,
    label: &str,
) -> Vec<String> {
    oracle_validated_profiled(validated, artifact, label, None)
}

#[derive(Default)]
struct OraclePhaseTiming {
    canonical_interpret: Duration,
    runtime_setup: Duration,
    spawn_admitted: Duration,
    wasm_run: Duration,
    heap_audit: Duration,
}

fn oracle_validated_profiled(
    validated: &ValidatedSafeWir,
    artifact: &witchy_wir::AdmittedCanonicalArtifact,
    label: &str,
    mut timing: Option<&mut OraclePhaseTiming>,
) -> Vec<String> {
    // Each nextest shard has its own process; install compiler-service natives
    // before running the canonical interpreter.
    witchy_interp::compiler_natives::install();
    let entry = artifact.entry().unwrap_or_else(|| panic!("{label}: a program has an entry"));
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let arguments = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
    // The oracle and the runtime both see the working directory and the
    // process environment (the dev grants `witchy run` derives), so an
    // example with `Dir`/`Env` parameters runs on both legs.
    let config = InterpreterConfig {
        console_trace: None,
        compiler_services: Some(witchy::compiler_oracle::call),
        max_steps: 20_000_000_000,
        fixture: witchy_wir::canonical_interpreter::OracleFixture {
            root: Some(std::path::PathBuf::from(".")),
            process_environment: true,
            dir_grants: Some(1),
            ..Default::default()
        },
    };
    let phase_started = timing.as_ref().map(|_| Instant::now());
    let run = interpret_entry_program(validated, function, arguments, &[], config)
        .unwrap_or_else(|e| panic!("{label}: oracle refused: {e:?}"));
    if let (Some(timing), Some(started)) = (timing.as_deref_mut(), phase_started) {
        timing.canonical_interpret = started.elapsed();
    }

    let phase_started = timing.as_ref().map(|_| Instant::now());
    // This corpus checks observable equivalence, admission, and heap ownership;
    // it does not assess Cranelift's Speed-tier code quality. Keep the same
    // sandbox and ABI while avoiding expensive optimization of large examples.
    let mut runtime = Runtime::batch_quick().expect("runtime");
    if let (Some(timing), Some(started)) = (timing.as_deref_mut(), phase_started) {
        timing.runtime_setup = started.elapsed();
    }
    let phase_started = timing.as_ref().map(|_| Instant::now());
    let mut vm = runtime
        .spawn_admitted(
            artifact,
            Capabilities {
                print: true,
                print_int: true,
                quiet: true,
                env: true,
                dir_root: Some(std::path::PathBuf::from(".")),
                dir_read: true,
                // Public but uncalled functions can retain Net imports even
                // when main has no Net parameter. Link them with an explicit
                // deny-all fixture: no destination or listener is authorized.
                net_allow: Some(Vec::new()),
                net_connect: true,
                net_listen: true,
                ..Default::default()
            },
            256,
        )
        .expect("spawn admitted artifact");
    if let (Some(timing), Some(started)) = (timing.as_deref_mut(), phase_started) {
        timing.spawn_admitted = started.elapsed();
    }
    let phase_started = timing.as_ref().map(|_| Instant::now());
    vm.run().unwrap_or_else(|e| panic!("{label}: wasm run failed: {e:#}"));
    if let (Some(timing), Some(started)) = (timing.as_deref_mut(), phase_started) {
        timing.wasm_run = started.elapsed();
    }
    let mut wasm_lines = vm.output();
    // The host prints an `Int`/`Float` result after the entry returns; the
    // oracle reports it as a result value instead.
    let rendered = match run.results.first() {
        Some(InterpreterValue::Int(n)) => Some(n.to_string()),
        Some(InterpreterValue::FloatBits(bits)) => Some(witchy_syntax::fmt::render_float(f64::from_bits(*bits))),
        _ => None,
    };
    if rendered.is_some() {
        let last = wasm_lines.pop();
        assert_eq!(last, rendered, "{label}: entry result differs");
    }
    assert_eq!(run.output, wasm_lines, "{label}: printed lines differ between oracle and wasm");
    assert_eq!(run.leaked_objects, 0, "{label}: the oracle saw objects survive the entry");
    let phase_started = timing.as_ref().map(|_| Instant::now());
    let leaks = vm.canonical_heap_leaks().expect("heap audit").expect("canonical module");
    if let (Some(timing), Some(started)) = (timing.as_deref_mut(), phase_started) {
        timing.heap_audit = started.elapsed();
    }
    assert_eq!(leaks, 0, "{label}: the wasm heap kept unreleased objects");
    run.output
}

#[test]
fn oracle_agrees_on_the_foundation_programs() {
    let programs: &[(&str, &str)] = &[
        ("print", "fn main(console: Console):\n    console.print(\"ok\")\n"),
        ("int result", "fn main() -> Int:\n    40 + 2\n"),
        ("float result", "fn main() -> Float:\n    1.5\n"),
        ("loops", "fn main(console: Console):\n    var i = 0\n    while i < 3:\n        console.print(\"tick\")\n        i = i + 1\n    for j in 0..2:\n        console.print(\"tock\")\n"),
        ("strings", "pub fn shout(name: String) -> String:\n    \"hi \" + name\n\nfn keep(own value: String) -> String:\n    value\n\nfn main(console: Console):\n    console.print(shout(\"x\"))\n    console.print(keep(\"ok\"))\n    console.print(\"${16} ${-7} ${0}\")\n"),
        ("lists", "pub fn dot(a: List(Int), b: List(Int)) -> Int:\n    var sum = 0\n    var i = 0\n    while i < a.length():\n        sum = sum + a[i] * b[i]\n        i = i + 1\n    sum\n\npub fn grow(placed: List(Int), col: Int) -> Int:\n    var next = placed\n    next.push(col)\n    next.length()\n\nfn main(console: Console):\n    let u = [1, 2, 3]\n    console.print(\"${dot(u, [4, 5, 6])} ${grow(u, 9)} ${u.length()}\")\n"),
        ("lists of strings", "pub fn join_all(xs: List(String)) -> String:\n    var out = \"\"\n    for s in xs:\n        out = out + s\n    out\n\nfn main(console: Console):\n    let xs = [\"a\", \"bb\", \"ccc\"]\n    console.print(join_all(xs))\n    console.print(xs[1])\n    var ys = [\"x\"]\n    ys.push(\"yy\")\n    console.print(\"${ys.length()}\")\n"),
        ("list patterns", "pub fn sum(xs: List(Int)) -> Int:\n    match xs:\n        [] -> 0\n        [head, ..tail] -> head + sum(tail)\n\nfn main(console: Console):\n    console.print(\"${sum([3, 7, 11])}\")\n"),
        ("joins", "pub fn pick(xs: List(Int), n: Int) -> List(Int):\n    if xs.length() == n:\n        xs\n    else:\n        var out = xs\n        out.push(n)\n        out\n\npub fn choose(flag: Bool, left: String, right: String) -> String:\n    if flag:\n        left\n    else:\n        right\n\nfn main(console: Console):\n    console.print(\"${pick([1], 1).length()} ${pick([1], 2).length()}\")\n    console.print(choose(true, \"l\", \"r\") + choose(false, \"l\", \"r\"))\n"),
        ("tail calls", "pub fn countdown(n: Int, acc: Int) -> Int:\n    if n == 0:\n        acc\n    else:\n        countdown(n - 1, acc + 1)\n\nfn main(console: Console):\n    console.print(\"${countdown(30000, 0)}\")\n"),
        ("recursion with list threading", "pub fn count(n: Int, placed: List(Int)) -> Int:\n    if placed.length() == n:\n        1\n    else:\n        var total = 0\n        var col = 0\n        while col < n:\n            if col % 2 == 0:\n                var next = placed\n                next.push(col)\n                total = total + count(n, next)\n            col = col + 1\n        total\n\nfn main(console: Console):\n    console.print(\"${count(3, [])}\")\n"),
    ];
    for (label, source) in programs {
        oracle(source, label);
    }
}

macro_rules! oracle_example_shards {
    ($($name:ident => $index:literal),+ $(,)?) => {
        const ORACLE_SHARD_IDS: &[usize] = &[$($index),+];
        $(
            #[test]
            fn $name() {
                assert!(ORACLE_SHARD_IDS.iter().copied().eq(0..ORACLE_SHARD_IDS.len()),
                    "every oracle shard must be registered exactly once");
                oracle_examples_shard($index, ORACLE_SHARD_IDS.len());
            }
        )+
    };
}

oracle_example_shards! {
    oracle_agrees_on_the_admitted_examples => 0,
    oracle_agrees_on_the_admitted_examples_shard_1 => 1,
    oracle_agrees_on_the_admitted_examples_shard_2 => 2,
    oracle_agrees_on_the_admitted_examples_shard_3 => 3,
    oracle_agrees_on_the_admitted_examples_shard_4 => 4,
    oracle_agrees_on_the_admitted_examples_shard_5 => 5,
    oracle_agrees_on_the_admitted_examples_shard_6 => 6,
    oracle_agrees_on_the_admitted_examples_shard_7 => 7,
    oracle_agrees_on_the_admitted_examples_shard_8 => 8,
    oracle_agrees_on_the_admitted_examples_shard_9 => 9,
    oracle_agrees_on_the_admitted_examples_shard_10 => 10,
    oracle_agrees_on_the_admitted_examples_shard_11 => 11,
    oracle_agrees_on_the_admitted_examples_shard_12 => 12,
    oracle_agrees_on_the_admitted_examples_shard_13 => 13,
    oracle_agrees_on_the_admitted_examples_shard_14 => 14,
    oracle_agrees_on_the_admitted_examples_shard_15 => 15,
}

fn oracle_examples_shard(shard: usize, shards: usize) {
    let root = super::manifest_dir();
    let requested = std::env::var("WITCHY_ORACLE_EXAMPLE").ok();
    let emit_timing = std::env::var_os("WITCHY_ORACLE_TIMING").is_some();
    let mut checked_any = false;
    for entry in std::fs::read_dir(root.join("examples")).expect("examples directory") {
        let dir = entry.expect("example entry").path();
        let Some(name) = dir.file_name().and_then(|n| n.to_str()).map(str::to_owned) else { continue };
        if requested.as_deref().is_some_and(|requested| requested != name) {
            continue;
        }
        // Stable assignment is independent of read_dir order and process hash
        // seeds. The registered shards partition the original corpus exactly;
        // no eligibility rule or execution assertion is relaxed. The task
        // examples each cost one to three minutes of compile time, so two of
        // them in one shard bound the whole suite's wall time; the byte-sum
        // buckets put `for_await` with `channels` and `worker_pool` with
        // `async_tasks`, and those two are pinned to otherwise light shards.
        // Examples whose DEDICATED three-leg test is strictly stronger than
        // this sweep -- independent expected output plus leak audits on both
        // the canonical interpreter and admitted Wasm -- are not run twice.
        // `channels` alone was ~15s here, duplicating the 17s
        // rfc0165_messages::canonical_channel_scheduler_three_legs.
        // `sweep_skips_only_examples_with_a_dedicated_leak_audited_test` keeps
        // this list honest.
        if requested.is_none() && SUBSUMED_BY_DEDICATED_TESTS.contains(&name.as_str()) {
            continue;
        }
        const PINNED: &[(&str, usize)] = &[("for_await", 1), ("worker_pool", 2)];
        let bucket = PINNED
            .iter()
            .find(|(pinned, _)| *pinned == name)
            .map(|(_, shard)| *shard % shards)
            .unwrap_or_else(|| name.bytes().fold(0usize, |sum, byte| sum.wrapping_add(usize::from(byte))) % shards);
        if bucket != shard {
            continue;
        }
        let file = dir.join("src").join(format!("{name}.witchy"));
        let Ok(source) = std::fs::read_to_string(&file) else { continue };
        if !source.contains("fn main") {
            continue;
        }
        eprintln!("oracle sweep: {}", file.display());
        // Only programs the foundation subset admits are the oracle's
        // business; refused programs are the census's.
        let total_started = emit_timing.then(Instant::now);
        let phase_started = emit_timing.then(Instant::now);
        let Ok(checked) = witchy::resolve_std_only_checked(&source) else { continue };
        let check = phase_started.map_or(Duration::ZERO, |started| started.elapsed());
        let phase_started = emit_timing.then(Instant::now);
        let mut observer_elapsed = Duration::ZERO;
        let mut oracle_phases = None;
        let outcome = codegen::compile_checked_module_artifact_with_observer(
            &checked, |validated, artifact| {
                // The semantic oracle deliberately has no network-I/O world.
                // Preserve the sweep's policy after full production admission.
                if artifact.entry().is_some_and(|entry| entry.capabilities().contains(
                    &witchy_wir::CanonicalEntryCapability::Net,
                )) {
                    return false;
                }
                let observer_started = emit_timing.then(Instant::now);
                let mut phases = OraclePhaseTiming::default();
                oracle_validated_profiled(
                    validated,
                    artifact,
                    &format!("examples/{name}"),
                    emit_timing.then_some(&mut phases),
                );
                observer_elapsed = observer_started
                    .map_or(Duration::ZERO, |started| started.elapsed());
                oracle_phases = emit_timing.then_some(phases);
                true
            });
        let admission = phase_started.map_or(Duration::ZERO, |started| started.elapsed());
        let codegen::LoweringOutcome::Lowered((_, ran)) = outcome
        else { continue };
        if let Some(phases) = oracle_phases {
            // The production observer runs only after lowering and admission;
            // its public boundary does not split those two phases further.
            let production_lowering_admission = admission.saturating_sub(observer_elapsed);
            let total = total_started.map_or(Duration::ZERO, |started| started.elapsed());
            eprintln!(
                "WITCHY_ORACLE_TIMING\texample={name}\tcheck_ms={:.3}\tproduction_lowering_admission_ms={:.3}\tcanonical_interpret_ms={:.3}\truntime_setup_ms={:.3}\tspawn_admitted_ms={:.3}\twasm_run_ms={:.3}\theap_audit_ms={:.3}\ttotal_ms={:.3}",
                check.as_secs_f64() * 1_000.0,
                production_lowering_admission.as_secs_f64() * 1_000.0,
                phases.canonical_interpret.as_secs_f64() * 1_000.0,
                phases.runtime_setup.as_secs_f64() * 1_000.0,
                phases.spawn_admitted.as_secs_f64() * 1_000.0,
                phases.wasm_run.as_secs_f64() * 1_000.0,
                phases.heap_audit.as_secs_f64() * 1_000.0,
                total.as_secs_f64() * 1_000.0,
            );
        }
        checked_any |= ran;
    }
    assert!(checked_any, "at least one requested example admits in shard {shard}");
}

fn assert_opaque_closure_three_legs(source: &str, expected: &[&str]) {
    assert_opaque_closure_three_legs_with_caps(source, expected,
        Capabilities { print: true, quiet: true, ..Default::default() });
}

fn assert_opaque_closure_three_legs_with_caps(source: &str, expected: &[&str], caps: Capabilities) {
    let checked = witchy::resolve_std_only_checked(source).expect("opaque closure source checks");
    let expected = expected.iter().map(|line| (*line).to_owned()).collect::<Vec<_>>();
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("opaque closure canonical kernel");
    let entry = artifact.entry().expect("entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked).expect("source-bound oracle validation");
    let capabilities = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
    let mut config = InterpreterConfig::default();
    config.fixture.net_grants = usize::from(caps.net_allow.is_some()) + caps.net_grants.len();
    let result = witchy_wir::canonical_interpreter::interpret_program(&validated, function, capabilities, config).expect("opaque closure oracle");
    assert_eq!(result.output, expected);
    assert_eq!(result.leaked_objects, 0);
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, caps, 256).expect("opaque closure admitted Wasm");
    vm.run().expect("opaque closure Wasm execution");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
}

#[test]
fn opaque_closure_plugin_host_expected_three_legs() {
    assert_opaque_closure_three_legs(
        include_str!("../../examples/plugin_host/src/plugin_host.witchy"),
        &["1 -> 12", "5 -> 20", "10 -> 30", "[log] ran the pipeline"],
    );
}

#[test]
fn opaque_closure_mixed_string_console_list_after_original_cleanup_three_legs() {
    assert_opaque_closure_three_legs(r#"
fn prefix(n: Int) -> String:
    "log: " + "${n}"

fn make(own text: String, console: Console) -> fn(String) -> ():
    let log = fn(message: String): console.print(text + message)
    console.print(text)
    log

fn main(console: Console):
    let logs = [make(prefix(7), console), make(prefix(8), console)]
    for log in logs:
        log(" A")
    for log in logs:
        log(" B")
"#, &["log: 7", "log: 8", "log: 7 A", "log: 8 A", "log: 7 B", "log: 8 B"]);
}

#[test]
fn canonical_layout_family_task_tuple_three_legs() {
    assert_opaque_closure_three_legs(r#"
import chan
async fn next(n: Int) -> Int:
    n + 1
async fn main(console: Console):
    let values = chan.par_map([4], fn(n): next(n)).await
    console.print("${values}")
"#, &["[5]"]);
}

#[test]
fn dynamic_reflection_example_expected_output_three_legs() {
    let source = include_str!("../../examples/dynamic_reflection/src/dynamic_reflection.witchy");
    // This harness links source bytes as module `main`; the CLI links the same
    // file as `dynamic_reflection`. The remaining expectations follow directly
    // from Counter(4), its Named impl, and the public add method called with 3.
    let expected = vec![
        "main.Counter".to_owned(),
        "add".to_owned(),
        "implements Named: true".to_owned(),
        "field value: 4".to_owned(),
        "result: 7".to_owned(),
    ];
    let checked = witchy::resolve_std_only_checked(source).expect("actual Dynamic example checks");
    assert!(checked.canonical_source().unwrap().dynamic_runtime_plan().is_some());
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("closed Dynamic source admits through the canonical route");
    let entry = artifact.entry().unwrap();
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked).expect("oracle validation");
    let capabilities = entry.capabilities().iter().copied()
        .map(InterpreterValue::entry_capability).collect();
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated, function, capabilities, Default::default(),
    ).expect("Dynamic oracle execution");
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0);
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
        print: true, quiet: true, ..Default::default()
    }, 256).expect("admitted Dynamic Wasm");
    vm.run().expect("Dynamic Wasm execution");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
}

#[test]
fn carried_state_unchanged_expected_three_legs() {
    assert_opaque_closure_three_legs_with_caps(
        include_str!("../../examples/carried_state/src/carried_state.witchy"),
        &["ok: counted rows in users", "denied: secrets is outside this handle (scoped to users)"],
        // Carry a real Net token, but authorize no network destination.
        Capabilities { print: true, quiet: true, net_allow: Some(Vec::new()), ..Default::default() },
    );
}

/// See the oracle sweep: each of these has a dedicated test that asserts an
/// independent expected output and zero leaks on both canonical engines.
const SUBSUMED_BY_DEDICATED_TESTS: &[&str] =
    &["channels", "carried_state", "plugin_host", "dynamic_reflection"];

#[test]
fn sweep_skips_only_examples_with_a_dedicated_leak_audited_test() {
    // Dropping an example from the sweep is only sound while a stronger test
    // still runs it. Each must be embedded by a test file that audits leaks
    // on both engines (`leaked_objects` for the SafeWIR interpreter,
    // `canonical_heap_leaks` for admitted Wasm).
    let misc = super::manifest_dir().join("tests/misc");
    for name in SUBSUMED_BY_DEDICATED_TESTS {
        let needle = format!("examples/{name}/src/{name}.witchy");
        let covered = std::fs::read_dir(&misc).expect("tests/misc").any(|entry| {
            let text = std::fs::read_to_string(entry.expect("entry").path()).unwrap_or_default();
            text.contains(&needle)
                && text.contains("leaked_objects")
                && text.contains("canonical_heap_leaks")
        });
        assert!(covered, "{name} is skipped by the oracle sweep but no leak-audited test embeds it");
    }
}
