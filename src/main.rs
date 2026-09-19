//! Native Witchy command-line application.
//!
//! This binary composes the compiler pipeline, canonical SafeWIR/Wasm runtime,
//! capability policy, project tooling, and bundled self-hosted commands. The
//! frozen source evaluator remains only for temporary comptime/build support.

// This crate is hand-indented, not rustfmt-managed. Clippy's "collapse nested
// conditionals" lints would rewrite explicit `if { if let ... }` nesting into
// `let`-chains without re-indenting, hurting readability; the nested form is an
// intentional style choice here.
#![allow(clippy::collapsible_if, clippy::collapsible_match, clippy::items_after_test_module)]
#![deny(unsafe_code)]

#[cfg(test)]
pub(crate) use witchy::comptime;
pub(crate) use witchy::artifact;
pub(crate) use witchy::ast;
pub(crate) use witchy::capabilities;
pub(crate) use witchy::codegen;
pub(crate) use witchy::format;
pub(crate) use witchy_caps::grants;
#[cfg(test)]
pub(crate) use witchy::interpreter;
pub(crate) use witchy::{enforce_performance_modes, is_entry_function};
pub(crate) use witchy::opt;
pub(crate) use witchy::pipeline;
mod cli;
mod commands;
#[cfg(test)]
pub(crate) use commands::build_steps::run_build_step_sandboxed;
#[cfg(test)]
pub(crate) use commands::sandbox::run_file_sandboxed;
#[cfg(test)]
pub(crate) use commands::test_runner::run_tests_in_file;
pub(crate) use commands::wasm_exec::{run_admitted_artifact, run_wasm_bytes, run_admitted_test_artifact, RUN_MEMORY_PAGES};
#[cfg(test)]
pub(crate) use commands::wasm_exec::run_wasm_file;
mod lsp;
mod source;
pub(crate) use witchy::parser;
mod idp;
pub(crate) use witchy::runtime;
pub(crate) use witchy::typeck;
pub(crate) use witchy::trusted_publisher;

pub(crate) use commands::capabilities::{
    report_capabilities, report_capability_diff, report_derived_csp, report_grant_check, report_grant_diff,
};
#[cfg(test)]
pub(crate) use commands::compile::{emit_wasm_file, emit_wat_file};
use commands::execution::run_checked_compiled;
#[cfg(test)]
pub(crate) use commands::execution::{parity_check, ParityOutcome};
#[cfg(test)]
pub(crate) use commands::frontend::check_file;
use runtime::Runtime;
pub(crate) use source::{
    bundled_module, link_file_checked, link_static_evaluation_file,
    link_file_checked_with_deps, link_test_file, linked_has_main, project_entry_file,
};
#[cfg(test)]
pub(crate) use source::link_file;
#[cfg(test)]
pub(crate) use source::link_file_with_deps;



fn main() -> wasmtime::Result<()> {
    commands::dispatch::run()
}

/// Run a `.witchy` file: resolve `import X` to sibling `X.witchy` files
/// (transitively), link, type-check, then run with root capabilities (Console
/// and a Dir rooted at the file's directory). Returns the program's output or a
/// diagnostic.
/// Time the canonical SafeWIR interpreter against admitted Wasm on a few workloads.
fn run_benchmarks() -> wasmtime::Result<()> {
    use std::time::Instant;
    use witchy_wir::canonical_interpreter::{interpret_program, InterpreterConfig};
    use witchy_wir::canonical_safewir::FunctionId;

    let fib = r#"
fn fib(n: Int) -> Int:
    if (n < 2):
        n
    else:
        (fib((n - 1)) + fib((n - 2)))

fn main() -> Int:
    fib(30)
"#;
    // Keep the accumulator bounded so this measures loop execution rather than
    // overflow handling.
    let loop_sum = r#"
fn main() -> Int:
    var sum = 0
    var i = 0
    while (i < 1000000):
        sum = ((sum + i) % 1000000)
        i = (i + 1)
    sum
"#;

    println!("== witchy benchmarks (avg ms/run) ==");
    for (name, src, runs) in [("fib(30)", fib, 5u32), ("loop_sum(1e6)", loop_sum, 5u32)] {
        let checked = witchy::resolve_std_only_checked(src).expect("resolve and check benchmark");
        let artifact = codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("the canonical path lowers this benchmark");
        let validated = codegen::validate_checked_module_for_oracle(&checked)
            .expect("validate benchmark for the canonical interpreter");
        let entry = artifact.entry().expect("benchmark entry");
        let function = FunctionId::new(
            entry.export().strip_prefix('f').expect("canonical entry export")
                .parse().expect("canonical function id"),
        );

        let canonical_start = Instant::now();
        for _ in 0..runs {
            interpret_program(&validated, function, Vec::new(), InterpreterConfig::default())
                .expect("canonical benchmark run");
        }
        let canonical_ms = canonical_start.elapsed().as_secs_f64() * 1000.0 / runs as f64;

        let mut runtime = Runtime::new().expect("runtime");
        let wasm_start = Instant::now();
        for _ in 0..runs {
            let mut vm = spawn_benchmark_iteration(&mut runtime, &artifact).expect("spawn");
            vm.run().expect("run");
        }
        let wasm_ms = wasm_start.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        println!(
            "{name:14}  canonical {canonical_ms:8.2} ms   wasm {wasm_ms:8.3} ms   ({:.0}x)",
            canonical_ms / wasm_ms
        );
    }
    Ok(())
}


/// Preserve the sealed entry and owner bridges for each fresh benchmark VM.
fn spawn_benchmark_iteration(
    runtime: &mut Runtime,
    artifact: &witchy_wir::AdmittedCanonicalArtifact,
) -> wasmtime::Result<runtime::Vm> {
    runtime.spawn_admitted(
        artifact,
        runtime::Capabilities {
            print: true,
            print_int: true,
            ..Default::default()
        },
        16,
    )
}

#[cfg(test)]
#[test]
fn remaining_internal_admitted_benchmark_owner_execution() {
    let source = r#"
fn make(console: Console, text: String) -> fn() -> ():
    fn(): console.print(text)

fn main(console: Console):
    let report = make(console, "benchmark capture")
    report()
    report()
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("checked benchmark fixture");
    let expected = vec!["benchmark capture".to_owned(); 2];
    assert_eq!(
        interpreter::run_checked_module(&checked, ".", Vec::new())
            .expect("independent source oracle"),
        expected,
    );
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admitted benchmark fixture");
    assert!(!artifact.owner_bridges().is_empty());
    let mut runtime = Runtime::new().expect("benchmark runtime");
    for _ in 0..2 {
        let mut vm = spawn_benchmark_iteration(&mut runtime, &artifact)
            .expect("fresh benchmark VM retains owner bridges");
        vm.run().expect("execute owner-bearing benchmark");
        assert_eq!(vm.output(), expected);
        assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
    }
}

/// Read a 32-byte Ed25519 signing seed from a file holding 64 hex characters.
fn load_signing_seed(path: &str) -> Result<[u8; 32], String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read `{path}`: {e}"))?;
    let hex = text.trim();
    if hex.len() != 64 {
        return Err(format!("seed must be 64 hex chars (32 bytes), got {}", hex.len()));
    }
    let mut seed = [0u8; 32];
    for (i, b) in seed.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| "seed is not valid hex".to_string())?;
    }
    Ok(seed)
}


// A no-args convenience wrapper over `execute_file_exit` (which the CLI run path
// uses to also get the process exit code), discarding the exit code — used by the
// test suite.
#[cfg(test)]
pub(crate) fn execute_file(path: &str, net_allow: Vec<String>) -> Result<Vec<String>, String> {
    execute_file_exit(path, net_allow, Vec::new(), Vec::new(), None, Vec::new())
        .map(|(output, _)| output)
}

/// Link, type-check, and run `path`, returning its output and the process exit
/// code (`main`'s `Int` return, else 0). `args` populate a `List(String)`
/// parameter; `signing_key` grants the root `Secret` capability.
pub(crate) fn execute_file_exit(
    path: &str,
    net_allow: Vec<String>,
    fetch_origins: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
    named_secrets: Vec<runtime::SecretGrant>,
) -> Result<(Vec<String>, i32), String> {
    let (checked, entry_stem) = link_file_checked(path)?;
    let linked = checked.module();
    enforce_performance_modes(linked, &entry_stem)?;

    // No `main` means there's nothing to run directly — but the file still
    // compiled. Explain rather than failing with "unknown function `main`".
    if !linked_has_main(linked) {
        let msg = format!(
            "`{entry_stem}` compiled OK — it's a library (no `main`); import it from another module."
        );
        return Ok((vec![msg], 0));
    }

    // One run path: the compiled (WASM) backend. `witchy run` and `witchy sandbox`
    // share one runtime, so dev == deploy by construction. The interpreter is only
    // the differential oracle (`witchy parity`) and the comptime evaluator — never
    // a user-program run path.
    run_checked_compiled(
        &checked,
        Vec::new(),
        Vec::new(),
        net_allow,
        fetch_origins,
        None,
        None,
        Vec::new(),
        args,
        signing_key,
        named_secrets,
        Vec::new(),
        false,
        witchy_confinement::EnforcementMode::Disabled,
    )
        .map(|(lines, code)| (lines, code.unwrap_or(0)))
}

/// End-to-end coverage: every shipped example must type-check and produce the
/// expected result (interpreted), or type-check and compile to valid WASM.
#[cfg(test)]
mod example_tests;

/// RFC-0072: verbatim diagnostic goldens over the full error surface (parse,
/// layout, link, type, capability, lowering-reject, runtime trap) via `insta`.
#[cfg(test)]
mod diagnostic_golden_tests;
