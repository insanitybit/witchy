//! Native Witchy command-line application.
//!
//! This binary composes the compiler pipeline, interpreter and Wasm runtime,
//! capability policy, project tooling, and bundled self-hosted commands.

// This crate is hand-indented, not rustfmt-managed. Clippy's "collapse nested
// conditionals" lints would rewrite explicit `if { if let ... }` nesting into
// `let`-chains without re-indenting, hurting readability; the nested form is an
// intentional style choice here.
#![allow(clippy::collapsible_if, clippy::collapsible_match, clippy::items_after_test_module)]
#![deny(unsafe_code)]

pub use witchy::analysis;
pub use witchy::artifact;
pub use witchy::ast;
pub use witchy::capabilities;
pub use witchy::codegen;
pub use witchy::comptime;
pub use witchy::doc;
pub use witchy::format;
pub use witchy::generators;
pub use witchy::grants;
pub use witchy::interpreter;
pub use witchy::linker;
pub use witchy::{enforce_performance_modes, is_entry_function};
pub use witchy::opt;
pub use witchy::pipeline;
mod cli;
mod commands;
#[cfg(test)]
pub(crate) use commands::build_steps::run_build_step_sandboxed;
#[cfg(test)]
pub(crate) use commands::sandbox::run_file_sandboxed;
#[cfg(test)]
pub(crate) use commands::test_runner::run_tests_in_file;
pub(crate) use commands::wasm_exec::{run_wasm_bytes, run_wasm_test_bytes, RUN_MEMORY_PAGES};
#[cfg(test)]
pub(crate) use commands::wasm_exec::run_wasm_file;
mod lsp;
mod source;
pub use witchy::native;
pub use witchy::net;
pub use witchy::parser;
mod idp;
pub use witchy::runtime;
pub use witchy::typeck;
pub use witchy::trusted_exe;
pub use witchy::value;
pub use witchy::wir;
pub use witchy::wir_encode;
pub use witchy::wir_helpers;
pub use witchy::wir_opt;

pub(crate) use commands::capabilities::{
    report_capabilities, report_capability_diff, report_grant_check,
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
    bundled_module, link_file, link_file_checked,
    link_file_checked_with_deps, link_file_with_mode, linked_has_main, project_entry_file,
};
#[cfg(test)]
pub(crate) use source::link_file_with_deps;



fn main() -> wasmtime::Result<()> {
    commands::dispatch::run()
}

/// Run a `.witchy` file: resolve `import X` to sibling `X.witchy` files
/// (transitively), link, type-check, then run with root capabilities (Console
/// and a Dir rooted at the file's directory). Returns the program's output or a
/// diagnostic.
/// Time the interpreter against the compiled (WASM) backend on a few workloads.
fn run_benchmarks() -> wasmtime::Result<()> {
    use std::time::Instant;

    fn interp_ms(src: &str, runs: u32) -> f64 {
        let start = Instant::now();
        for _ in 0..runs {
            interpreter::run(src).expect("interpreter run");
        }
        start.elapsed().as_secs_f64() * 1000.0 / runs as f64
    }

    fn compiled_ms(src: &str, runs: u32) -> f64 {
        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this benchmark");
        let mut rt = Runtime::new().expect("runtime");
        let start = Instant::now();
        for _ in 0..runs {
            let mut vm = rt
                .spawn(
                    &bytes,
                    runtime::Capabilities {
                        print: true,
                        print_int: true,
                        ..Default::default()
                    },
                    16,
                )
                .expect("spawn");
            vm.run().expect("run");
        }
        start.elapsed().as_secs_f64() * 1000.0 / runs as f64
    }

    let fib = r#"
fn fib(n: Int) -> Int:
    if (n < 2):
        n
    else:
        (fib((n - 1)) + fib((n - 2)))

fn main() -> Int:
    fib(30)
"#;
    // The accumulator is kept under 10^6 (`% 1000000`) so it stays well within
    // the compiled backend's 32-bit `Int`; otherwise this sum would overflow in
    // the compiled run (which wraps at 2^31) while the i64 interpreter would not,
    // making the two columns incomparable. (Compiled `Int` is i32, a deliberate
    // divergence from the interpreter's i64 — see `ty_kind` in codegen.)
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
        let i = interp_ms(src, runs);
        let c = compiled_ms(src, runs);
        println!(
            "{name:14}  interpreter {i:8.2} ms   compiled {c:8.3} ms   ({:.0}x)",
            i / c
        );
    }
    Ok(())
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
    execute_file_exit(path, net_allow, Vec::new(), None, Vec::new()).map(|(output, _)| output)
}

/// Link, type-check, and run `path`, returning its output and the process exit
/// code (`main`'s `Int` return, else 0). `args` populate a `List(String)`
/// parameter; `signing_key` grants the root `Secret` capability.
pub(crate) fn execute_file_exit(
    path: &str,
    net_allow: Vec<String>,
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
    run_checked_compiled(&checked, Vec::new(), Vec::new(), net_allow, args, signing_key, named_secrets, Vec::new(), false, false)
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
