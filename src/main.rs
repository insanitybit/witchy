//! Witchy runtime spike — proving the core thesis: an actor in an isolated
//! WASM VM can do nothing beyond the capabilities it was explicitly granted.
//!
//! The "actors" here are hand-written WebAssembly standing in for compiled
//! witchy code; the point of the spike is the security substrate, not the
//! language surface yet.

// This crate is hand-indented, not rustfmt-managed. Clippy's "collapse nested
// conditionals" lints would rewrite explicit `if { if let ... }` nesting into
// `let`-chains without re-indenting, hurting readability; the nested form is an
// intentional style choice here.
#![allow(clippy::collapsible_if, clippy::collapsible_match)]

mod actor_system;
mod aliases;
mod ast;
mod capabilities;
mod codegen;
mod consts;
mod format;
mod generators;
mod interpreter;
mod lexer;
mod linker;
mod lsp;
mod native;
mod parser;
mod pm;
mod records;
mod runtime;
mod rustgen;
mod traits;
mod typeck;

use std::time::Duration;

use runtime::{Capabilities, Runtime};

/// A well-behaved actor that was granted `print`.
const GREETER: &str = r#"
(module
  (import "witchy" "print" (func $print (param i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "hello from inside the sandbox\n")
  (func (export "run")
    (call $print (i32.const 0) (i32.const 30))))
"#;

/// A "malicious library": it *tries* to import `print`, but we will grant it
/// nothing. It must fail to even come up.
const MALICIOUS: &str = r#"
(module
  (import "witchy" "print" (func $print (param i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "I am exfiltrating your secrets\n")
  (func (export "run")
    (call $print (i32.const 0) (i32.const 31))))
"#;

/// An actor that receives one message and prints it. Needs `print`; `recv` is
/// intrinsic.
const LOGGER: &str = r#"
(module
  (import "witchy" "recv" (func $recv (param i32 i32) (result i32)))
  (import "witchy" "print" (func $print (param i32 i32)))
  (memory (export "memory") 1)
  (func (export "run")
    (local $n i32)
    (local.set $n (call $recv (i32.const 256) (i32.const 256)))
    (if (i32.ge_s (local.get $n) (i32.const 0))
      (then (call $print (i32.const 256) (local.get $n))))))
"#;

/// A greedy actor: it declares 4 pages of initial memory. We will cap it at 1,
/// so it must be denied at instantiation.
const GREEDY: &str = r#"
(module
  (memory (export "memory") 4)
  (func (export "run")))
"#;

/// A runaway actor: an infinite loop that never yields. The scheduler must be
/// able to preempt it.
const RUNAWAY: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "run")
    (loop $forever (br $forever))))
"#;

/// An actor that sends one message to a target id. Needs `send`. The target id
/// is filled in at spawn time so the demo doesn't depend on id arithmetic.
fn sender_src(target: u32) -> String {
    let text = "ping from the sender actor";
    let len = text.len() + 1; // +1 for the trailing newline byte
    format!(
        r#"
(module
  (import "witchy" "send" (func $send (param i32 i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "{text}\n")
  (func (export "run")
    (call $send (i32.const {target}) (i32.const 0) (i32.const {len}))))
"#
    )
}

/// One-screen overview of the command-line interface, shown for bare `witchy`.
fn print_usage() {
    println!(
        "\
witchy — a capability-secure language with twin interpreter and WASM backends

USAGE:
    witchy [--net <host:port>]... <file.witchy>   run a program
    witchy check    <file.witchy>                 type-check without running
    witchy parity   <file.witchy>                 run on both backends, confirm identical output
    witchy sandbox  <file.witchy>                 compile and run in a VM granted exactly its footprint
    witchy native [-o out] <file.witchy>          compile to a native binary via rustc/LLVM, then run it
    witchy emit-wat <file.witchy>                 print the compiled WebAssembly text (the module sandbox runs)
    witchy emit-rust <file.witchy>                print the native (Rust) transpilation
    witchy caps     <file.witchy>                 report the capability footprint
    witchy caps-diff <old.witchy> <new.witchy>    fail if the footprint widened
    witchy fmt [--check] <file.witchy>            reformat in place (--check: verify only, exit 1 if not)
    witchy lsp                                    run the language server
    witchy demo                                   built-in capability/runtime demonstration

Package commands (add, build, publish, ...) are also available."
    );
}

fn main() -> wasmtime::Result<()> {
    // `witchy caps <file>` reports the program's host-capability footprint.
    if std::env::args().nth(1).as_deref() == Some("caps") {
        let Some(path) = std::env::args().nth(2) else {
            eprintln!("usage: witchy caps <file>");
            std::process::exit(1);
        };
        if let Err(e) = report_capabilities(&path) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // `witchy caps-diff <old> <new>` reports whether the newer version widens the
    // capability footprint, exiting 2 on a widening so it can gate CI/installs.
    if std::env::args().nth(1).as_deref() == Some("caps-diff") {
        let (Some(old), Some(new)) = (std::env::args().nth(2), std::env::args().nth(3)) else {
            eprintln!("usage: witchy caps-diff <old.witchy> <new.witchy>");
            std::process::exit(1);
        };
        match report_capability_diff(&old, &new) {
            Ok(widened) => std::process::exit(if widened { 2 } else { 0 }),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }
    // `witchy lsp` starts the language server (stdio), used by editor extensions.
    if std::env::args().nth(1).as_deref() == Some("lsp") {
        if let Err(e) = lsp::run() {
            eprintln!("witchy lsp: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // `witchy check <file>` parses, links, and type-checks without running —
    // exits non-zero on any error. Validates programs you can't run (servers).
    if std::env::args().nth(1).as_deref() == Some("check") {
        let Some(path) = std::env::args().nth(2) else {
            eprintln!("usage: witchy check <file.witchy>");
            std::process::exit(1);
        };
        match check_file(&path) {
            Ok(()) => println!("{path}: ok"),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    // `witchy parity <file>` runs the program on both the interpreter and the
    // compiled WASM backend and confirms they produce identical output. (Named
    // `parity`, not `verify` — `verify` is the package-manager's TUF/signature
    // command.)
    if std::env::args().nth(1).as_deref() == Some("parity") {
        let Some(path) = std::env::args().nth(2) else {
            eprintln!("usage: witchy parity <file.witchy>");
            std::process::exit(1);
        };
        if let Err(e) = verify_file(&path) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // `witchy sandbox <file>` compiles the program to WASM and runs it in the
    // capability-sandboxed VM, granted exactly its declared footprint.
    if std::env::args().nth(1).as_deref() == Some("sandbox") {
        let Some(path) = std::env::args().nth(2) else {
            eprintln!("usage: witchy sandbox <file.witchy>");
            std::process::exit(1);
        };
        match run_file_sandboxed(&path) {
            Ok(lines) => {
                for line in lines {
                    println!("{line}");
                }
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    // `witchy emit-wat <file>` prints the compiled WebAssembly text — the same
    // module `sandbox` runs — for inspecting/optimizing the generated code.
    if std::env::args().nth(1).as_deref() == Some("emit-wat") {
        let Some(path) = std::env::args().nth(2) else {
            eprintln!("usage: witchy emit-wat <file.witchy>");
            std::process::exit(1);
        };
        match emit_wat_file(&path) {
            Ok(wat) => print!("{wat}"),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    // `witchy emit-rust <file>` prints the native (Rust) transpilation.
    if std::env::args().nth(1).as_deref() == Some("emit-rust") {
        let Some(path) = std::env::args().nth(2) else {
            eprintln!("usage: witchy emit-rust <file.witchy>");
            std::process::exit(1);
        };
        match emit_rust_file(&path) {
            Ok(src) => print!("{src}"),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    // `witchy native [-o out] <file>` compiles a program to a native binary via
    // rustc/LLVM and runs it (or, with `-o`, just writes the binary).
    if std::env::args().nth(1).as_deref() == Some("native") {
        let args: Vec<String> = std::env::args().skip(2).collect();
        // `-o <out> <file>` builds a binary; `<file> [args...]` builds and runs,
        // forwarding the trailing arguments to the program (its argv).
        let (out, path, prog_args) = match args.as_slice() {
            [o, out, path] if o == "-o" => (Some(out.clone()), Some(path.clone()), Vec::new()),
            [path, rest @ ..] => (None, Some(path.clone()), rest.to_vec()),
            [] => (None, None, Vec::new()),
        };
        let Some(path) = path else {
            eprintln!("usage: witchy native [-o <out>] <file.witchy> [args...]");
            std::process::exit(1);
        };
        match build_native(&path, out.as_deref(), &prog_args) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    // `witchy fmt <file>` rewrites a source file in canonical brace-free form.
    if std::env::args().nth(1).as_deref() == Some("fmt") {
        // `witchy fmt --check <file>` verifies formatting without rewriting (for
        // CI): exit 0 if already canonical, 1 if it would change.
        let check = std::env::args().nth(2).as_deref() == Some("--check");
        let path = std::env::args().nth(if check { 3 } else { 2 });
        let Some(path) = path else {
            eprintln!("usage: witchy fmt [--check] <file.witchy>");
            std::process::exit(1);
        };
        match std::fs::read_to_string(&path) {
            Ok(src) => match format::reformat(&src) {
                Some(out) => {
                    if check {
                        if out != src {
                            eprintln!("witchy fmt: `{path}` is not formatted");
                            std::process::exit(1);
                        }
                    } else if let Err(e) = std::fs::write(&path, out) {
                        eprintln!("witchy fmt: {e}");
                        std::process::exit(1);
                    }
                }
                None => {
                    eprintln!("witchy fmt: cannot format `{path}` (parse error or unsupported construct)");
                    std::process::exit(1);
                }
            },
            Err(e) => {
                eprintln!("witchy fmt: cannot read `{path}`: {e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    // `witchy --bench` compares interpreter vs compiled execution.
    if std::env::args().nth(1).as_deref() == Some("--bench") {
        return run_benchmarks();
    }
    // coven package-manager subcommands (`witchy add`, `build`, `publish`, ...)
    // are checked before the file/`--net` runner so they intercept first.
    if let Some(a1) = std::env::args().nth(1) {
        if pm::cli::is_command(&a1) {
            let args: Vec<String> = std::env::args().skip(1).collect();
            if let Err(e) = pm::cli::run(&args) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
            return Ok(());
        }
    }
    // `witchy [--net <host:port>]... <file.witchy>` runs a program, granting the
    // listed hosts to its `Net` capability (the host decides what authority to
    // hand over). With no file argument, run the demos.
    {
        let mut net_allow: Vec<String> = Vec::new();
        let mut file: Option<String> = None;
        let mut prog_args: Vec<String> = Vec::new();
        let mut signing_key: Option<[u8; 32]> = None;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            if file.is_some() {
                // Everything after the program file is the program's own argv —
                // passed through verbatim (flags here belong to the program).
                prog_args.push(arg);
            } else if let Some(host) = arg.strip_prefix("--net=") {
                net_allow.push(host.to_string());
            } else if arg == "--net" {
                match args.next() {
                    Some(host) => net_allow.push(host),
                    None => {
                        eprintln!("--net requires a <host:port> argument");
                        std::process::exit(1);
                    }
                }
            } else if arg == "--signing-key" || arg.starts_with("--signing-key=") {
                // Grant the root `SigningKey` from a file holding a 64-hex-char
                // (32-byte) Ed25519 seed — the host decides what key to hand over.
                let path = match arg.strip_prefix("--signing-key=") {
                    Some(p) => p.to_string(),
                    None => match args.next() {
                        Some(p) => p,
                        None => {
                            eprintln!("--signing-key requires a <seed-file> argument");
                            std::process::exit(1);
                        }
                    },
                };
                match load_signing_seed(&path) {
                    Ok(seed) => signing_key = Some(seed),
                    Err(e) => {
                        eprintln!("--signing-key: {e}");
                        std::process::exit(1);
                    }
                }
            } else {
                file = Some(arg);
            }
        }
        match file.as_deref() {
            // Standard help flags show the usage overview.
            Some("--help" | "-h" | "help") => {
                print_usage();
                return Ok(());
            }
            // `witchy demo` runs the built-in capability/runtime demonstration
            // below; fall through to it.
            Some("demo") => {}
            Some(path) if !std::path::Path::new(path).is_file() => {
                // A first argument that is neither a known subcommand nor a real
                // file is almost always a mistyped command — point at usage.
                eprintln!("witchy: `{path}` is not a known command or readable file");
                eprintln!("run `witchy` with no arguments for usage");
                std::process::exit(1);
            }
            Some(path) => {
                match execute_file_exit(path, net_allow, prog_args, signing_key) {
                    Ok((output, code)) => {
                        for line in output {
                            println!("{line}");
                        }
                        // `main`'s `Int` return is the process exit status.
                        if code != 0 {
                            std::process::exit(code);
                        }
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                }
                return Ok(());
            }
            // Bare `witchy` (or only flags): show usage rather than the demo.
            None => {
                print_usage();
                return Ok(());
            }
        }
    }

    let mut rt = Runtime::new()?;

    println!("== M2: capability gating ==");

    // Granted the `print` capability — works.
    let mut greeter = rt.spawn(GREETER, Capabilities { print: true, send: false, print_int: false, quiet: false }, 4)?;
    greeter.run()?;

    // Granted nothing — must fail to instantiate because `witchy.print` is not
    // linked into its VM.
    match rt.spawn(MALICIOUS, Capabilities::none(), 4) {
        Ok(_) => println!("!! SECURITY FAILURE: ungranted actor was allowed to instantiate"),
        Err(e) => println!("DENIED (as designed): {e}"),
    }

    println!("\n== M3: message passing across isolated VMs ==");

    // Logger can print + recv, but cannot send.
    let mut logger = rt.spawn(LOGGER, Capabilities { print: true, send: false, print_int: false, quiet: false }, 4)?;
    // Sender can only send.
    let mut sender = rt.spawn(
        sender_src(logger.id),
        Capabilities { print: false, send: true, print_int: false, quiet: false },
        4,
    )?;

    sender.run()?; // delivers a message into the logger's mailbox
    logger.run()?; // receives it (into ITS own memory) and prints it

    println!("\n== M4: containment ==");

    // Memory budget: the greedy actor wants 4 pages but is capped at 1.
    match rt.spawn(GREEDY, Capabilities::none(), 1) {
        Ok(_) => println!("!! BUDGET FAILURE: over-budget actor was allowed to start"),
        Err(e) => println!("memory budget enforced: {e}"),
    }

    // Preemption: the runaway actor loops forever; the scheduler interrupts it.
    let mut runaway = rt.spawn(RUNAWAY, Capabilities::none(), 4)?;
    match rt.run_with_budget(&mut runaway, Duration::from_millis(50)) {
        Ok(_) => println!("!! PREEMPTION FAILURE: runaway actor finished on its own"),
        Err(e) => {
            let reason = e
                .downcast_ref::<wasmtime::Trap>()
                .map(|t| t.to_string())
                .unwrap_or_else(|| e.to_string());
            println!("PREEMPTED (as designed): {reason}");
        }
    }

    run_witchy("witchy language (interpreter)", include_str!("../examples/hello.witchy"));
    run_witchy("witchy mutable value semantics", include_str!("../examples/mutate.witchy"));
    run_witchy("witchy ownership (sink)", include_str!("../examples/ownership.witchy"));
    run_witchy("witchy features combined", include_str!("../examples/commands.witchy"));
    run_witchy("witchy fizzbuzz (while, %, if/else)", include_str!("../examples/fizzbuzz.witchy"));
    run_witchy("witchy tuples (multiple return values)", include_str!("../examples/tuples.witchy"));
    run_witchy("witchy generics (swap any pair)", include_str!("../examples/generics.witchy"));
    run_witchy("witchy generic ADTs (Result)", include_str!("../examples/result.witchy"));
    run_witchy("witchy ? error propagation", include_str!("../examples/try.witchy"));
    run_witchy("witchy for-in loops over lists", include_str!("../examples/loops.witchy"));
    run_witchy("witchy list patterns (head/tail)", include_str!("../examples/listmatch.witchy"));
    run_witchy("witchy records (named fields)", include_str!("../examples/records.witchy"));
    run_witchy("witchy record update", include_str!("../examples/record_update.witchy"));
    run_witchy("witchy expression evaluator (recursive ADT)", include_str!("../examples/eval.witchy"));
    run_witchy("witchy bank (records + lists + Result)", include_str!("../examples/bank.witchy"));
    run_witchy("witchy higher-order functions (closures)", include_str!("../examples/higher_order.witchy"));
    run_witchy("witchy list combinators (map/filter via push)", include_str!("../examples/list_ops.witchy"));
    run_witchy("witchy dictionaries (word count)", include_str!("../examples/wordcount.witchy"));
    run_witchy("witchy dict iteration (values/pairs)", include_str!("../examples/inventory.witchy"));
    run_witchy("witchy early return (guard clauses)", include_str!("../examples/guard.witchy"));
    run_witchy("witchy negative-literal patterns", include_str!("../examples/signs.witchy"));
    run_witchy("witchy string slicing (substring/index_of)", include_str!("../examples/parse_kv.witchy"));
    run_witchy("witchy actors", include_str!("../examples/actors.witchy"));
    run_witchy("witchy filesystem capability", include_str!("../examples/files.witchy"));
    run_compiled(&mut rt, "witchy compiled to WASM (ints)", include_str!("../examples/compute.witchy"));
    run_compiled(&mut rt, "witchy compiled to WASM (ADTs)", include_str!("../examples/shapes.witchy"));
    run_compiled(&mut rt, "witchy compiled to WASM (record field access)", include_str!("../examples/record_compiled.witchy"));
    run_compiled(&mut rt, "witchy compiled to WASM (strings)", include_str!("../examples/strings.witchy"));
    run_compiled_actor(&mut rt, "witchy actor compiled to its own WASM VM", include_str!("../examples/counter.witchy"));
    run_actor_system("witchy compiled actors messaging", include_str!("../examples/mailbox.witchy"));
    run_net_demo("witchy network capability");
    run_program_demo(
        "witchy modules (import)",
        &[
            ("strutil", include_str!("../examples/strutil.witchy")),
            ("app", include_str!("../examples/app.witchy")),
        ],
        "app",
    );
    run_program_demo(
        "witchy standard library (import list)",
        &[
            ("list", include_str!("../std/list.witchy")),
            ("std_demo", include_str!("../examples/std_demo.witchy")),
        ],
        "std_demo",
    );
    run_compiled_program(
        &mut rt,
        "witchy list combinators compiled to WASM (map/filter/fold/sort_by)",
        &[
            ("list", include_str!("../std/list.witchy")),
            ("list_pipeline", include_str!("../examples/list_pipeline.witchy")),
        ],
        "list_pipeline",
    );
    run_program_demo(
        "witchy list search/slice (contains/index_of/take/drop)",
        &[
            ("list", include_str!("../std/list.witchy")),
            ("list_more", include_str!("../examples/list_more.witchy")),
        ],
        "list_more",
    );
    run_program_demo(
        "witchy list zip/enumerate",
        &[
            ("list", include_str!("../std/list.witchy")),
            ("string", include_str!("../std/string.witchy")),
            ("zip", include_str!("../examples/zip.witchy")),
        ],
        "zip",
    );
    run_program_demo(
        "witchy list any/all (predicates)",
        &[
            ("list", include_str!("../std/list.witchy")),
            ("predicates", include_str!("../examples/predicates.witchy")),
        ],
        "predicates",
    );
    run_program_demo(
        "witchy text processing (split/map/join)",
        &[
            ("list", include_str!("../std/list.witchy")),
            ("string", include_str!("../std/string.witchy")),
            ("text", include_str!("../examples/text.witchy")),
        ],
        "text",
    );
    run_program_demo(
        "witchy sorting (sort_by with a comparator)",
        &[
            ("list", include_str!("../std/list.witchy")),
            ("string", include_str!("../std/string.witchy")),
            ("sort", include_str!("../examples/sort.witchy")),
        ],
        "sort",
    );
    run_program_demo(
        "witchy standard library (import math)",
        &[
            ("math", include_str!("../std/math.witchy")),
            ("math_demo", include_str!("../examples/math_demo.witchy")),
        ],
        "math_demo",
    );
    run_program_demo(
        "witchy float math (sqrt + fabs/fmin/fmax)",
        &[
            ("math", include_str!("../std/math.witchy")),
            ("floats", include_str!("../examples/floats.witchy")),
        ],
        "floats",
    );
    run_program_demo(
        "witchy standard Result (import result + ?)",
        &[
            ("result", include_str!("../std/result.witchy")),
            (
                "rclient",
                r#"
import result

fn checked_div(a: Int, b: Int) -> Result(Int, String):
    match b:
        0 -> Err("divide by zero")
        _ -> Ok((a / b))

fn compute(x: Int, y: Int) -> Result(Int, String):
    let q = (checked_div(x, y))?
    Ok((q + 1))

fn main(console: Console):
    print(console, int_to_string(result.unwrap_or(compute(10, 2), (0 - 1))))
    print(console, int_to_string(result.unwrap_or(compute(10, 0), (0 - 1))))
"#,
            ),
        ],
        "rclient",
    );
    run_program_demo(
        "witchy standard Option (import option)",
        &[
            ("option", include_str!("../std/option.witchy")),
            ("option_std", include_str!("../examples/option_std.witchy")),
        ],
        "option_std",
    );

    println!("\nspike OK");
    Ok(())
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
        let wat = codegen::compile_module(&module).expect("compile");
        let mut rt = Runtime::new().expect("runtime");
        let start = Instant::now();
        for _ in 0..runs {
            let mut actor = rt
                .spawn(
                    wat.as_bytes(),
                    runtime::Capabilities {
                        print: true,
                        print_int: true,
                        ..Default::default()
                    },
                    16,
                )
                .expect("spawn");
            actor.run().expect("run");
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

/// Source for a bundled standard-library module, shipped with the compiler so
/// `import list` works without a local file. A local file of the same name
/// takes precedence (see `execute_file`).
fn bundled_module(name: &str) -> Option<&'static str> {
    crate::linker::std_source(name)
}

/// Parse and link a source file, resolving each `import` from a sibling
/// `<name>.witchy` (preferred) or the bundled std. Returns the linked module and
/// the entry module's stem. Shared by `execute_file` and `check_file`.
fn link_file(path: &str) -> Result<(ast::Module, String), String> {
    use std::collections::{HashSet, VecDeque};
    use std::path::{Path, PathBuf};

    let entry_path = Path::new(path);
    let dir: &Path = entry_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let entry_stem = entry_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("invalid file name: {path}"))?
        .to_string();

    let mut modules: Vec<(String, ast::Module)> = Vec::new();
    let mut loaded: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, PathBuf)> = VecDeque::new();
    queue.push_back((entry_stem.clone(), entry_path.to_path_buf()));

    while let Some((name, p)) = queue.pop_front() {
        if !loaded.insert(name.clone()) {
            continue; // already loaded (cycle-safe)
        }
        // A local `<name>.witchy` wins; otherwise fall back to a bundled
        // standard-library module (e.g. `import list`).
        let src = match std::fs::read_to_string(&p) {
            Ok(s) => s,
            Err(e) => match bundled_module(&name) {
                Some(s) => s.to_string(),
                None => {
                    // A misspelled `import` of a std module gets a suggestion.
                    let hint = if name != entry_stem {
                        crate::linker::closest_std_module(&name)
                            .map(|m| format!(" — did you mean `import {m}`?"))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    return Err(format!("cannot read `{}`: {e}{hint}", p.display()));
                }
            },
        };
        let module = parser::parse_module(&src).map_err(|e| format!("{name}: {e}"))?;
        for imp in &module.imports {
            if !loaded.contains(imp) {
                queue.push_back((imp.clone(), dir.join(format!("{imp}.witchy"))));
            }
        }
        modules.push((name, module));
    }

    let linked = linker::link(modules, &entry_stem).map_err(|e| e.to_string())?;
    Ok((linked, entry_stem))
}

/// Parse, link, and type-check a file WITHOUT running it (`witchy check`). Useful
/// for CI and for validating programs you don't want to run — e.g. servers,
/// which never return.
fn check_file(path: &str) -> Result<(), String> {
    let (linked, _stem) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())
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

// Convenience wrapper (no command-line args) — used by the test suite; the CLI
// run path calls `execute_file_exit` to also get the process exit code.
#[cfg_attr(not(test), allow(dead_code))]
fn execute_file(path: &str, net_allow: Vec<String>) -> Result<Vec<String>, String> {
    execute_file_args(path, net_allow, Vec::new(), None)
}

/// Like [`execute_file`] but with command-line `args` and an optional signing
/// key, discarding the process exit code (used by the test suite).
#[cfg_attr(not(test), allow(dead_code))]
fn execute_file_args(
    path: &str,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
) -> Result<Vec<String>, String> {
    execute_file_exit(path, net_allow, args, signing_key).map(|(output, _)| output)
}

/// Link, type-check, and run `path`, returning its output and the process exit
/// code (`main`'s `Int` return, else 0). `args` populate a `List(String)`
/// parameter; `signing_key` grants the root `SigningKey` capability.
fn execute_file_exit(
    path: &str,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
) -> Result<(Vec<String>, i32), String> {
    use std::path::Path;
    let (linked, entry_stem) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;

    // No `main` means there's nothing to run directly — but the file still
    // compiled. Explain rather than failing with "unknown function `main`".
    let has_main = linked
        .items
        .iter()
        .any(|it| matches!(it, ast::Item::Function(f) if f.name == "main"));
    if !has_main {
        let actors: Vec<&str> = linked
            .items
            .iter()
            .filter_map(|it| match it {
                ast::Item::Actor(a) => Some(a.name.as_str()),
                _ => None,
            })
            .collect();
        let msg = if actors.is_empty() {
            format!("`{entry_stem}` compiled OK — it's a library (no `main`); import it from another module.")
        } else {
            format!(
                "`{entry_stem}` compiled OK — it defines actor(s) {} but no `main`; drive them from a `main` or the compiled runtime.",
                actors.join(", ")
            )
        };
        return Ok((vec![msg], 0));
    }

    // The root `Dir` capability is anchored at the current directory (the same
    // root the demos use), independent of where the source file lives.
    interpreter::run_module_exit(linked, Path::new("."), net_allow, args, signing_key)
        .map_err(|e| e.to_string())
}

/// Run a program on BOTH backends — the tree-walking interpreter and compiled
/// WebAssembly — and confirm they produce identical output. Witchy's
/// dual-backend equivalence is normally an internal test invariant; `witchy
/// verify` surfaces it as a guarantee you can check on your own code.
fn verify_file(path: &str) -> Result<(), String> {
    use std::path::Path;
    let (linked, _stem) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    let has_main = linked
        .items
        .iter()
        .any(|it| matches!(it, ast::Item::Function(f) if f.name == "main"));
    if !has_main {
        return Err(format!("`{path}` has no `main` to run"));
    }
    // Compile first (borrows `linked`), then run the interpreter (consumes it).
    let wat = codegen::compile_module(&linked)
        .map_err(|e| format!("cannot compile to WASM (an interpreter-only feature?): {e}"))?;
    let interp =
        interpreter::run_module(linked, Path::new("."), Vec::new()).map_err(|e| e.to_string())?;
    let compiled = run_wat_capture(&wat)?;
    if interp == compiled {
        println!(
            "\u{2713} {path}: interpreter and compiled WASM agree ({} line(s) of output)",
            interp.len()
        );
        Ok(())
    } else {
        Err(format!(
            "\u{2717} {path}: the two backends DIVERGE\n  interpreter: {interp:?}\n  compiled:    {compiled:?}"
        ))
    }
}

/// Compile a program to WASM and run it inside the capability-sandboxed VM,
/// granting exactly the authority its footprint declares. The compiled sandbox
/// currently links only the console (`print`) host, so it supports Console-only
/// (or pure) programs; anything needing `Dir`/`Net` is reported, not run.
/// Returns the program's output lines.
/// Compile a program to WebAssembly text (WAT) and return it — the same module
/// `sandbox` would run. For inspecting and optimizing the generated code.
fn emit_wat_file(path: &str) -> Result<String, String> {
    let (linked, _stem) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    codegen::compile_module(&linked)
        .map_err(|e| format!("cannot compile to WASM (an interpreter-only feature?): {e}"))
}

/// Transpile a program to native Rust source (the input to the native backend).
fn emit_rust_file(path: &str) -> Result<String, String> {
    let (linked, _stem) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    // Desugar traits/impls and monomorphize `where`-bounded generics into plain
    // concrete functions — the same lowering the interpreter and WASM backends
    // apply — so the native backend sees ordinary functions, not trait dispatch.
    let lowered = crate::traits::lower(linked);
    rustgen::transpile_module(&lowered)
}

/// Compile a program to a native binary via rustc/LLVM. With `out`, the binary
/// is written there; otherwise it's built to a temp path and run immediately.
fn build_native(path: &str, out: Option<&str>, prog_args: &[String]) -> Result<(), String> {
    let src = emit_rust_file(path)?;
    let stem = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("prog");
    let tmp = std::env::temp_dir().join(format!("witchy_native_{}_{stem}", std::process::id()));
    let rs = tmp.with_extension("rs");
    std::fs::write(&rs, &src).map_err(|e| format!("cannot write `{}`: {e}", rs.display()))?;
    let bin = match out {
        Some(o) => std::path::PathBuf::from(o),
        None => tmp.clone(),
    };
    // -O is LLVM's optimizer; overflow-checks off matches the wasm backend's
    // wrapping i64 arithmetic (and avoids debug-mode overflow panics).
    let status = std::process::Command::new("rustc")
        .args(["-O", "-C", "overflow-checks=off", "--edition", "2021"])
        .arg(&rs)
        .arg("-o")
        .arg(&bin)
        .status()
        .map_err(|e| format!("cannot run rustc (is it installed?): {e}"))?;
    let _ = std::fs::remove_file(&rs);
    if !status.success() {
        return Err("native backend: rustc failed to compile the generated Rust".into());
    }
    if out.is_none() {
        let run = std::process::Command::new(&bin)
            .args(prog_args)
            .status()
            .map_err(|e| format!("cannot run native binary: {e}"))?;
        let _ = std::fs::remove_file(&bin);
        if !run.success() {
            std::process::exit(run.code().unwrap_or(1));
        }
    }
    Ok(())
}

fn run_file_sandboxed(path: &str) -> Result<Vec<String>, String> {
    let (linked, _stem) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    let has_main = linked
        .items
        .iter()
        .any(|it| matches!(it, ast::Item::Function(f) if f.name == "main"));
    if !has_main {
        return Err(format!("`{path}` has no `main` to run"));
    }
    let footprint = capabilities::analyze(&linked);
    let unsupported: Vec<&str> = footprint
        .total
        .keys()
        .copied()
        .filter(|c| *c != "Console")
        .collect();
    if !unsupported.is_empty() {
        return Err(format!(
            "the compiled sandbox supports Console-only programs for now; `{path}` also needs {}",
            unsupported.join(", ")
        ));
    }
    let wat = codegen::compile_module(&linked)
        .map_err(|e| format!("cannot compile to WASM (an interpreter-only feature?): {e}"))?;
    eprintln!(
        "sandboxing `{path}` \u{2014} granted exactly: {}",
        capabilities::show_caps(&footprint.total)
    );
    run_wat_capture(&wat)
}

/// Instantiate a compiled WAT module under print/print_int authority, run its
/// `run` export, and return the captured output lines.
/// Linear-memory cap (64 KiB pages) for a run-to-completion program: 1 GiB.
/// wasmtime grows memory lazily, so this is just a ceiling, not a reservation —
/// it lets real programs (lists, strings, recursion) allocate freely while
/// still bounding a runaway. (The tiny per-actor caps used by the scheduler are
/// a separate, deliberate resource-limit demonstration.)
const RUN_MEMORY_PAGES: usize = 16384;

fn run_wat_capture(wat: &str) -> Result<Vec<String>, String> {
    use crate::runtime::{Capabilities, Runtime};
    // Run-to-completion: no scheduler, so use the non-preempting engine, which
    // omits the per-backedge epoch check and runs tight loops at full speed.
    let mut rt = Runtime::batch().map_err(|e| e.to_string())?;
    let mut actor = rt
        .spawn(
            wat.as_bytes(),
            Capabilities {
                print: true,
                print_int: true,
                quiet: true,
                ..Default::default()
            },
            RUN_MEMORY_PAGES,
        )
        .map_err(|e| e.to_string())?;
    actor.run().map_err(|e| e.to_string())?;
    Ok(actor.output())
}

/// Read, parse, and compute the host-capability footprint of a source file.
fn analyze_file(path: &str) -> Result<capabilities::Footprint, String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("cannot read `{path}`: {e}"))?;
    let module = parser::parse_module(&src).map_err(|e| e.to_string())?;
    Ok(capabilities::analyze(&module))
}

/// Print the host-capability footprint of a single source file: which of
/// `Console`/`Dir`/`Net` each entry point requires, and the union.
fn report_capabilities(path: &str) -> Result<(), String> {
    let fp = analyze_file(path)?;
    let show = capabilities::show_caps;
    println!("Host-capability footprint of {path}:");
    let width = fp
        .entries
        .iter()
        .map(|e| e.name.len())
        .max()
        .unwrap_or(0)
        .max("total".len());
    for e in &fp.entries {
        let refined = if e.brands.is_empty() {
            String::new()
        } else {
            let names: Vec<&str> = e.brands.iter().map(String::as_str).collect();
            format!("  (refined: {})", names.join(", "))
        };
        println!("  {:<width$}  {}{}", e.name, show(&e.capabilities), refined);
    }
    println!("  {:<width$}  {}", "total", show(&fp.total));
    Ok(())
}

/// Compare the capability footprints of two versions of a module and report any
/// *widening* — host authority the newer version demands that the older did not.
/// Returns whether it widened so the caller can fail the supply-chain gate. This
/// is what makes `witchy` dependency updates auditable: unlike Go, where a
/// version bump can silently start touching the network, a widening is visible
/// and blockable here.
fn report_capability_diff(old_path: &str, new_path: &str) -> Result<bool, String> {
    let old = analyze_file(old_path)?;
    let new = analyze_file(new_path)?;
    let d = capabilities::diff(&old, &new);
    println!("Capability footprint diff {old_path} -> {new_path}:");
    println!("  old total:  {}", capabilities::show_caps(&old.total));
    println!("  new total:  {}", capabilities::show_caps(&new.total));
    println!("  added:      {}", capabilities::show_caps(&d.added));
    println!("  removed:    {}", capabilities::show_caps(&d.removed));
    let join = |s: &std::collections::BTreeSet<String>| {
        if s.is_empty() {
            "(none)".to_string()
        } else {
            s.iter().cloned().collect::<Vec<_>>().join(", ")
        }
    };
    if !d.refinements_dropped.is_empty() || !d.refinements_gained.is_empty() {
        println!(
            "  refinements: dropped {} / gained {}",
            join(&d.refinements_dropped),
            join(&d.refinements_gained)
        );
    }
    if d.widened() {
        println!(
            "WIDENING: the newer version demands new host authority ({}). Review before trusting.",
            capabilities::show_caps(&d.added)
        );
    } else if !d.refinements_dropped.is_empty() {
        // Same host authority, but a brand was dropped — a confined capability
        // loosened to its bare form. Not a widening, but an intent change.
        println!(
            "OK on authority, but a refinement was dropped ({}): a confined capability loosened to its bare form. Worth a look.",
            join(&d.refinements_dropped)
        );
    } else {
        println!("OK: no widening — the newer version demands no new host authority.");
    }
    Ok(d.widened())
}

/// Parse, link, and run a multi-module program through the interpreter.
fn run_program_demo(title: &str, sources: &[(&str, &str)], entry: &str) {
    println!("\n== {title} ==");
    match interpreter::run_program(sources, entry) {
        Ok(out) => {
            for line in out {
                println!("{line}");
            }
        }
        Err(e) => println!("error: {e}"),
    }
}

/// Demonstrate the Net capability against a loopback echo server: a granted
/// address can be reached; an address outside the allow-list is denied.
fn run_net_demo(title: &str) {
    use std::io::{BufRead, BufReader, Write};
    println!("\n== {title} ==");
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => {
            println!("could not bind loopback: {e}");
            return;
        }
    };
    let addr = listener.local_addr().unwrap().to_string();
    let server = std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let mut r = BufReader::new(stream);
            let mut line = String::new();
            let _ = r.read_line(&mut line);
            let _ = r.get_mut().write_all(format!("echo: {line}").as_bytes());
        }
    });

    let program = format!(
        r#"
        fn main(console: Console, net: Net) {{
          let s = connect(net, "{addr}")
          send_line(s, "hello over the wire")
          print(console, recv_line(s))
        }}
    "#
    );
    match interpreter::run_with(&program, ".", vec![addr.clone()]) {
        Ok(out) => {
            for line in out {
                println!("{line}");
            }
        }
        Err(e) => println!("error: {e}"),
    }
    server.join().ok();

    let denied = r#"
fn main(console: Console, net: Net):
    let s = connect(net, "10.255.255.1:80")
    send_line(s, "x")
"#;
    if let Err(e) = interpreter::run_with(denied, ".", vec![addr]) {
        println!("DENIED outside the allow-list (as designed): {e}");
    }
}

/// Compile a program's actors and run them on the actor system, wiring a
/// Forwarder's Subject to a Printer and relaying a few messages — compiled
/// actors sending to each other across their WASM VMs.
fn run_actor_system(title: &str, src: &str) {
    println!("\n== {title} ==");
    if let Err(e) = typeck::check_str(src) {
        println!("{e}");
        return;
    }
    let module = match parser::parse_module(src) {
        Ok(m) => m,
        Err(e) => {
            println!("{e}");
            return;
        }
    };
    let (actors, tags) = match codegen::compile_program(&module) {
        Ok(v) => v,
        Err(e) => {
            println!("{e}");
            return;
        }
    };
    let mut sys = actor_system::System::new(tags);
    let (mut printer, mut fwd) = (None, None);
    for (name, wat) in &actors {
        match sys.spawn(wat) {
            Ok(id) => {
                if name == "Printer" {
                    printer = Some(id);
                } else if name == "Forwarder" {
                    fwd = Some(id);
                }
            }
            Err(e) => {
                println!("spawn failed: {e}");
                return;
            }
        }
    }
    let (Some(printer), Some(fwd)) = (printer, fwd) else {
        println!("expected Printer and Forwarder actors");
        return;
    };
    let _ = sys.set_subject(fwd, "target", printer);
    for n in 1..=3 {
        let _ = sys.send(fwd, "Relay", n);
    }
    for line in sys.output() {
        println!("{line}");
    }
}

/// Compile an actor to its own WASM module and run it on the runtime: each
/// `Tick` is delivered by invoking the actor's exported handler, state persists
/// in a WASM global across messages, and without the capability the compiled
/// module cannot instantiate.
fn run_compiled_actor(rt: &mut Runtime, title: &str, src: &str) {
    println!("\n== {title} ==");
    if let Err(e) = typeck::check_str(src) {
        println!("{e}");
        return;
    }
    let module = match parser::parse_module(src) {
        Ok(m) => m,
        Err(e) => {
            println!("{e}");
            return;
        }
    };
    let actor = module.items.iter().find_map(|i| match i {
        ast::Item::Actor(a) => Some(a),
        _ => None,
    });
    let Some(actor) = actor else {
        println!("no actor to compile");
        return;
    };
    let wat = match codegen::compile_actor_module(actor) {
        Ok(w) => w,
        Err(e) => {
            println!("{e}");
            return;
        }
    };

    // Granted the Console capability (host `print`): deliver three Ticks.
    match rt.spawn(
        wat.as_bytes(),
        Capabilities {
            print: true,
            ..Default::default()
        },
        4,
    ) {
        Ok(mut counter) => {
            for _ in 0..3 {
                if let Err(e) = counter.invoke("Tick") {
                    println!("error: {e}");
                    break;
                }
            }
        }
        Err(e) => println!("spawn failed: {e}"),
    }

    // Denied: the compiled actor cannot even instantiate.
    match rt.spawn(wat.as_bytes(), Capabilities::none(), 4) {
        Ok(_) => println!("!! SECURITY FAILURE: actor ran without the capability"),
        Err(e) => println!("DENIED without capability (as designed): {e}"),
    }
}

/// End-to-end coverage: every shipped example must type-check and produce the
/// expected result (interpreted), or type-check and compile to valid WASM.
#[cfg(test)]
mod example_tests {
    use crate::{ast, codegen, interpreter, parser, typeck};
    use wasmtime::{Engine, Module};

    fn interp(src: &str) -> Vec<String> {
        assert!(
            typeck::check_str(src).is_ok(),
            "type error: {:?}",
            typeck::check_str(src)
        );
        interpreter::run(src).expect("should run")
    }

    /// Link a single-`main` source (pulling in any imported std module) and run
    /// it on the interpreter — the path that resolves `import crypto`.
    fn link_run(src: &str) -> Vec<String> {
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::linker::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        interpreter::run_module(linked, ".", Vec::new()).expect("run")
    }

    /// `crypto.sha256` — a native intrinsic of the `crypto` module, *not* a global
    /// builtin — matches the canonical SHA-256 vectors, requires `import crypto`,
    /// and computes the same digest on the interpreter and the compiled WASM
    /// backend (the host fills the guest-allocated result string).
    #[test]
    fn crypto_sha256_matches_known_vectors() {
        let out = link_run(
            "import crypto\nfn main(console: Console):\n    print(console, crypto.sha256(\"\"))\n    print(console, crypto.sha256(\"abc\"))\n",
        );
        assert_eq!(
            out,
            vec![
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ]
        );
        // No global builtin: bare `sha256` (without `import crypto`) is unknown.
        assert!(typeck::check_str("fn main(c: Console):\n    print(c, sha256(\"x\"))\n").is_err());
        // The compiled WASM backend computes the same digest (the host fills the
        // 64-byte result the guest pre-allocated) — interpreter↔WASM parity.
        let module = parser::parse_module(
            "import crypto\nfn main(console: Console):\n    print(console, crypto.sha256(\"abc\"))\n",
        )
        .expect("parse");
        let linked = crate::linker::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let wat = codegen::compile_module(&linked).expect("compile");
        assert_eq!(
            crate::run_wat_capture(&wat).expect("wasm run"),
            vec!["ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"]
        );
    }

    /// The native `Dir` capability: confined file I/O. Reads a file via a
    /// cwd-rooted Dir, compiled and run in a temp directory.
    #[test]
    fn native_backend_dir_capability() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_dir_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("data.txt"), "file contents").expect("write data");
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "fn main(dir: Dir, console: Console):\n    print(console, read(dir, \"data.txt\"))\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains("w_dir_read(&"), "expected a confined read");
        assert!(rust.contains("escapes the Dir capability"), "expected the confinement helper");
        // Compile + run in the temp dir (the Dir is rooted at the cwd).
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(s) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(s.success(), "rustc should compile the Dir program");
            let out = std::process::Command::new(&bin)
                .current_dir(&dir)
                .output()
                .expect("run");
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "file contents");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The Env capability (env vars), command-line args (`List(String)` main
    /// param), and an `Int`-returning main as the process exit code.
    #[test]
    fn native_backend_env_args_exit_code() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_env_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "fn main(env: Env, args: List(String), console: Console) -> Int:\n    print(console, f\"{length(args)} args\")\n    match get_env(env, \"WX_TEST\"):\n        Some(v) -> print(console, v)\n        None -> print(console, \"none\")\n    7\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains("std::env::var("), "expected get_env");
        assert!(rust.contains("std::env::args()"), "expected argv");
        assert!(rust.contains("std::process::exit("), "expected exit-code main");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(s) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(s.success(), "rustc should compile the Env program");
            let out = std::process::Command::new(&bin)
                .args(["alpha", "beta"])
                .env("WX_TEST", "hi")
                .output()
                .expect("run");
            assert_eq!(String::from_utf8_lossy(&out.stdout), "2 args\nhi\n");
            assert_eq!(out.status.code(), Some(7), "exit code");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `inout` parameters (Hylo write-back): a function with inout params returns
    /// its final values; statement-position calls reassign their arguments.
    #[test]
    fn native_backend_inout_writeback() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_inout_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "fn bump(inout n: Int):\n    n = n + 1\n\nfn swap(inout a: Int, inout b: Int):\n    let t = a\n    a = b\n    b = t\n\nfn main(console: Console):\n    var x = 3\n    var y = 7\n    bump(x)\n    swap(x, y)\n    print(console, f\"{x} {y}\")\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains("-> i64"), "expected inout fn to return its value");
        assert!(rust.contains("= w_"), "expected a write-back assignment");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(s) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(s.success(), "rustc should compile the inout program");
            let out = std::process::Command::new(&bin).output().expect("run");
            // x: 3 -> bump -> 4; swap(4,7) -> x=7, y=4.
            assert_eq!(String::from_utf8_lossy(&out.stdout), "7 4\n");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An `inout` call used as a value (not a statement) must be rejected, not
    /// silently mislowered — the call's witchy result is unit.
    #[test]
    fn native_backend_inout_as_value_errors() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_inoutv_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "fn bump(inout n: Int):\n    n = n + 1\n\nfn main(console: Console):\n    var x = 1\n    let y = bump(x)\n    print(console, f\"{x}\")\n",
        )
        .expect("write src");
        let err = crate::emit_rust_file(src.to_str().unwrap()).unwrap_err();
        assert!(err.contains("must be a statement"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A single-variant positional type (a newtype like `type Box: Box(Int,
    /// String)`) is registered and usable as a parameter/return type.
    #[test]
    fn native_backend_single_variant_type() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_sv_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "type Box:\n    Box(Int, String)\n\nfn label(b: Box) -> String:\n    match b:\n        Box(n, s) -> f\"{s}={n}\"\n\nfn main(console: Console):\n    print(console, label(Box(5, \"x\")))\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains("enum Box"), "single-variant type should register: {rust}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The Net capability: a confined TCP client connects to an in-process echo
    /// server (allow-listed via `WITCHY_NET`), sends a line, and prints the echo —
    /// byte-identical to the interpreter's socket path.
    #[test]
    fn native_backend_net_client() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                // Read the whole line, echo it, then hold the connection open
                // until the client closes — so it never sees a premature reset.
                let mut buf = Vec::new();
                let mut tmp = [0u8; 64];
                loop {
                    match s.read(&mut tmp) {
                        Ok(0) | Err(_) => break,
                        Ok(k) => {
                            buf.extend_from_slice(&tmp[..k]);
                            if buf.contains(&b'\n') {
                                break;
                            }
                        }
                    }
                }
                let _ = s.write_all(&buf);
                let _ = s.flush();
                let _ = s.read(&mut tmp);
            }
        });
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_net_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            format!("fn main(net: Net, console: Console):\n    let s = connect(net, \"{addr}\")\n    send_line(s, \"ping\")\n    print(console, recv_line(s))\n    close(s)\n"),
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains("w_net_connect("), "expected a confined connect");
        assert!(rust.contains("is not permitted by this Net capability"), "expected the allow-list guard");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        let mut ran = false;
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the Net program");
            let out = std::process::Command::new(&bin)
                .env("WITCHY_NET", &addr)
                .output()
                .expect("run");
            assert_eq!(String::from_utf8_lossy(&out.stdout), "ping\n");
            ran = true;
        }
        if !ran {
            // No rustc available: make sure the server thread isn't left blocking.
            let _ = std::net::TcpStream::connect(&addr);
        }
        let _ = server.join();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Lazy iterators (std/iter): the mutually-recursive `Step`/`Iter` types
    /// (Iter holds a closure returning Step) compile; an infinite `count_from`
    /// composed with map/take/collect terminates correctly.
    #[test]
    fn native_backend_lazy_iterators() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_iter_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "import iter\n\nfn main(console: Console):\n    let squares = iter.collect(iter.take(iter.map(iter.count_from(1), fn(n: Int): n * n), 4))\n    print(console, f\"{squares}\")\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains("enum Step") && rust.contains("enum Iter"), "expected Step/Iter emitted");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the lazy-iterator program");
            let out = std::process::Command::new(&bin).output().expect("run");
            assert_eq!(String::from_utf8_lossy(&out.stdout), "[1, 4, 9, 16]\n");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A list of distinct named functions unifies into `Vec<Rc<dyn Fn>>` (each
    /// fn-reference is coerced to the trait object), then each is applied.
    #[test]
    fn native_backend_heterogeneous_fn_list() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_fnl_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "fn double(n: Int) -> Int:\n    n * 2\n\nfn inc(n: Int) -> Int:\n    n + 1\n\nfn run_all(fns: List(fn(Int) -> Int), x: Int) -> Int:\n    var acc = x\n    for f in fns:\n        acc = f(acc)\n    acc\n\nfn main(console: Console):\n    print(console, int_to_string(run_all([double, inc], 5)))\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains("Rc<dyn Fn(i64) -> i64>"), "expected fn-refs coerced to Rc<dyn Fn>");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the fn-list program");
            let out = std::process::Command::new(&bin).output().expect("run");
            assert_eq!(String::from_utf8_lossy(&out.stdout), "11\n");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `match` scrutinee reused in an arm body (after the match destructures it)
    /// is cloned, so the original stays usable — value semantics (closes bst).
    #[test]
    fn native_backend_match_scrutinee_reuse() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_scr_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "type Tree:\n    Leaf\n    Node(Tree, Int, Tree)\n\nfn count(t: Tree) -> Int:\n    match t:\n        Leaf -> 1\n        Node(l, v, r) -> 1 + count(l) + count(r)\n\nfn root_plus_count(t: Tree) -> Int:\n    match t:\n        Leaf -> 0\n        Node(l, v, r) -> v + count(t)\n\nfn main(console: Console):\n    let t = Node(Node(Leaf, 1, Leaf), 5, Node(Leaf, 2, Leaf))\n    print(console, int_to_string(root_plus_count(t)))\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the scrutinee-reuse program");
            let out = std::process::Command::new(&bin).output().expect("run");
            assert_eq!(String::from_utf8_lossy(&out.stdout), "12\n");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The capability-secure web framework (examples/serve_hello.witchy) compiles
    /// natively: handlers are stored as `Rc<dyn Fn>` in a router and dispatched.
    #[test]
    fn native_backend_web_server_compiles() {
        let rust = crate::emit_rust_file("examples/serve_hello.witchy").expect("transpile");
        assert!(rust.contains("Rc<dyn Fn"), "expected handlers stored as Rc<dyn Fn>");
        // The two richer web examples must transpile too: serve_api exercises
        // tower-style middleware (`.layer`) plus the std `json` module, and
        // serve_static a Dir-confined file handler routed over HTTP.
        let api = crate::emit_rust_file("examples/serve_api.witchy").expect("transpile serve_api");
        assert!(api.contains("Rc<dyn Fn"), "serve_api: handlers/layers as Rc<dyn Fn>");
        let stat =
            crate::emit_rust_file("examples/serve_static.witchy").expect("transpile serve_static");
        assert!(stat.contains("w_dir_"), "serve_static: a Dir-confined read");
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_web_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "the web server should compile to a valid native binary");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The web framework end-to-end on the native backend: an inline router with a
    /// static route, a `:name` path-param route returning std-`json`, and a
    /// tower-style `.layer` middleware. We compile it, run the binary as a real
    /// server (allow-listed via `WITCHY_NET`), make HTTP requests over TCP, and
    /// assert the responses — plus that the middleware actually executed (it logged
    /// each request, which a handler structurally could not do without a Console).
    #[test]
    fn native_backend_web_server_end_to_end() {
        use std::io::{Read, Write};
        // Discover a free port, then let the generated server bind it.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let addr = format!("127.0.0.1:{port}");
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_web_e2e_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            format!(
                "import http\nimport server\nimport json\n\nfn logging(console: Console) -> fn(fn(Request) -> Response) -> fn(Request) -> Response:\n    fn(next: fn(Request) -> Response):\n        fn(req: Request):\n            print(console, f\"{{server.method(req)}} {{server.path(req)}}\")\n            next(req)\n\nfn home(req: Request) -> Response:\n    server.html(200, \"<h1>witchy web</h1>\")\n\nfn greet(req: Request) -> Response:\n    server.json_value(200, JsonObject([(\"hello\", JsonString(server.param(req, \"name\")))]))\n\nfn main(console: Console, net: Net):\n    let app = server.router()\n        .get(\"/\", home)\n        .get(\"/hello/:name\", greet)\n        .layer(logging(console))\n    print(console, \"ready\")\n    server.serve(net, \"{addr}\", app)\n"
            ),
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        else {
            // No rustc available in this environment: nothing to run.
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        assert!(st.success(), "the web server should compile");
        let logf = dir.join("out.log");
        let log_handle = std::fs::File::create(&logf).unwrap();
        let mut child = std::process::Command::new(&bin)
            .env("WITCHY_NET", &addr)
            .stdout(log_handle)
            .spawn()
            .expect("spawn server");
        // Wait for the listener to come up.
        let mut up = false;
        for _ in 0..60 {
            if std::net::TcpStream::connect(&addr).is_ok() {
                up = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if !up {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&dir);
            panic!("server never started listening on {addr}");
        }
        let get = |path: &str| -> String {
            let mut s = std::net::TcpStream::connect(&addr).unwrap();
            s.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
            s.write_all(
                format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes(),
            )
            .unwrap();
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf); // read to EOF (or the 2s timeout)
            buf
        };
        let r_home = get("/");
        let r_greet = get("/hello/witchy");
        let _ = child.kill();
        let _ = child.wait();
        let logged = std::fs::read_to_string(&logf).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            r_home.contains("200") && r_home.contains("<h1>witchy web</h1>"),
            "GET / => {r_home}"
        );
        assert!(
            r_greet.contains("{\"hello\":\"witchy\"}"),
            "GET /hello/:name should return std-json => {r_greet}"
        );
        assert!(
            logged.contains("GET /") && logged.contains("GET /hello/witchy"),
            "the .layer middleware should have logged both requests => {logged:?}"
        );
    }

    /// A static **file server** end-to-end, demonstrating the `Dir` capability over
    /// HTTP (this is examples/serve_static.witchy's story, exercised in the suite).
    /// The handler captures a `Dir` confined to the served root, so it serves files
    /// inside that subtree but a path-traversal request (`../secret`, `../../etc/...`)
    /// is denied — the confinement turns an escape into a 404, never a leak and never
    /// a crash. We send raw, un-normalized request bytes over TCP so the *server*,
    /// not a well-behaved client, is what must reject the escape.
    #[test]
    fn native_backend_file_server_end_to_end() {
        use std::io::{Read, Write};
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let addr = format!("127.0.0.1:{port}");
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_fs_e2e_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        // The served root is a *subdirectory*; a secret sits one level above it, so
        // a `../` escape would have to climb out of the Dir capability to reach it.
        let served = dir.join("served");
        std::fs::create_dir_all(&served).unwrap();
        std::fs::write(served.join("greeting.txt"), "hello from a confined Dir").unwrap();
        std::fs::write(dir.join("secret.txt"), "TOP SECRET").unwrap();
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            format!(
                "import http\nimport server\n\nfn file_server(dir: Dir) -> fn(Request) -> Response:\n    fn(req: Request): serve_file(dir, server.param(req, \"path\"))\n\nfn serve_file(dir: Dir, p: String) -> Response:\n    if exists(dir, p):\n        server.text(200, read(dir, p))\n    else:\n        server.not_found()\n\nfn main(console: Console, net: Net, root: Dir):\n    let app = server.router()\n        .get(\"/files/*path\", file_server(root))\n    print(console, \"ready\")\n    server.serve(net, \"{addr}\", app)\n"
            ),
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        else {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        assert!(st.success(), "the file server should compile");
        // Run with the served subdir as cwd: the native `root: Dir` is the cwd, so
        // the capability is confined to `served/` and cannot reach `secret.txt`.
        let mut child = std::process::Command::new(&bin)
            .current_dir(&served)
            .env("WITCHY_NET", &addr)
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn file server");
        let mut up = false;
        for _ in 0..60 {
            if std::net::TcpStream::connect(&addr).is_ok() {
                up = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if !up {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&dir);
            panic!("file server never started listening on {addr}");
        }
        let get = |path: &str| -> String {
            let mut s = std::net::TcpStream::connect(&addr).unwrap();
            s.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
            // Raw, un-normalized bytes — the server must reject any traversal itself.
            s.write_all(
                format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes(),
            )
            .unwrap();
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            buf
        };
        let ok = get("/files/greeting.txt");
        let missing = get("/files/missing.txt");
        let escape_up = get("/files/../secret.txt");
        let escape_abs = get("/files/../../etc/hosts");
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            ok.contains("200") && ok.contains("hello from a confined Dir"),
            "a file inside the served root should be served => {ok}"
        );
        assert!(missing.contains("404"), "a missing file should be a 404 => {missing}");
        // The escapes must be denied: 404, and crucially the secret must NOT leak.
        assert!(
            escape_up.contains("404") && !escape_up.contains("TOP SECRET"),
            "`../secret.txt` must be denied by the Dir capability, not leaked => {escape_up}"
        );
        assert!(
            escape_abs.contains("404") && !escape_abs.to_lowercase().contains("localhost"),
            "`../../etc/hosts` must be denied, not leaked => {escape_abs}"
        );
    }

    /// Closures are `Rc<dyn Fn>`, so a closure can be reused — passed to several
    /// calls / applied repeatedly — without a move error (the refcount clones).
    #[test]
    fn native_backend_closure_reuse() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_creuse_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "import list\n\nfn apply_twice(f: fn(Int) -> Int, x: Int) -> Int:\n    f(f(x))\n\nfn main(console: Console):\n    let add1 = fn(n: Int): n + 1\n    print(console, int_to_string(apply_twice(add1, 10)))\n    let ys = list.map([1, 2, 3], add1)\n    print(console, int_to_string(list.sum(ys)))\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains("Rc<dyn Fn"), "expected Rc<dyn Fn> closures");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the closure-reuse program");
            let out = std::process::Command::new(&bin).output().expect("run");
            assert_eq!(String::from_utf8_lossy(&out.stdout), "12\n9\n");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Generic structural `==`: a function comparing values of a type variable
    /// gets a `PartialEq` bound (only when it actually uses `==`/`!=`).
    #[test]
    fn native_backend_generic_equality() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_geq_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "fn count_eq(xs: List(a), target: a) -> Int:\n    var n = 0\n    for x in xs:\n        if x == target:\n            n = n + 1\n    n\n\nfn main(console: Console):\n    print(console, int_to_string(count_eq([1, 2, 2, 3, 2], 2)))\n    print(console, int_to_string(count_eq([\"a\", \"b\", \"a\"], \"a\")))\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains("PartialEq"), "expected a PartialEq bound on the generic");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the generic-equality program");
            let out = std::process::Command::new(&bin).output().expect("run");
            assert_eq!(String::from_utf8_lossy(&out.stdout), "3\n2\n");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Trait methods + `where`-bounded generics run natively, byte-identical to the
    /// interpreter. `traits::lower` monomorphizes a bounded generic into concrete
    /// specializations (`largest__Int`, `largest__Version`) and resolves each
    /// trait-method call to the right impl — including trait *default* methods
    /// (Ord's `greater` is defined in terms of `compare`) and impls for user types.
    /// The native backend now applies that lowering and emits the receiver param
    /// `self` as a legal Rust identifier. Exercises the three std traits: Ord
    /// (largest), Eq (member/index_of), and Show (show_list over user types).
    #[test]
    fn native_backend_trait_methods() {
        let cases = [
            ("largest", "examples/largest.witchy", "largest number: 100\nlatest version: 2.0\n"),
            ("equality", "examples/equality.witchy", "true\n2\nfalse\ntrue\n"),
            ("display", "examples/display.witchy", "[1, 2, 3]\n[(0, 0), (3, 4)]\n"),
        ];
        for (tag, file, expected) in cases {
            let rust = crate::emit_rust_file(file).expect("transpile trait program");
            // A bounded generic is gone (monomorphized); the lowered, concrete
            // specialization carries no `where` clause and no bare `self` value.
            assert!(!rust.contains("where"), "{tag}: bounds should be discharged");
            // The interpreter is the source of truth; native must match it.
            let (linked, _) = crate::link_file(file).expect("link");
            let interp = interpreter::run_module(linked, ".", Vec::new())
                .expect("interp run")
                .join("\n");
            assert_eq!(format!("{interp}\n"), expected, "{tag}: interp output drifted");
            let pid = std::process::id();
            let dir = std::env::temp_dir().join(format!("witchy_trait_{tag}_{pid}"));
            let _ = std::fs::create_dir_all(&dir);
            let rs = dir.join("prog.rs");
            let bin = dir.join("prog_bin");
            std::fs::write(&rs, &rust).unwrap();
            if let Ok(st) = std::process::Command::new("rustc")
                .args(["-O", "-C", "overflow-checks=off", "--edition", "2021"])
                .arg(&rs)
                .arg("-o")
                .arg(&bin)
                .status()
            {
                assert!(st.success(), "{tag}: rustc should compile the lowered trait program");
                let out = std::process::Command::new(&bin).output().expect("run");
                assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "{tag}: native output");
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// Actors run natively, byte-identical to the interpreter, via the same
    /// deterministic model: a single FIFO mailbox, run-to-completion handlers,
    /// per-actor isolated state. `actors.witchy` checks message ordering (a relayed
    /// message lands last) and that a handler mutates and persists `var` state;
    /// `files.witchy` checks a capability (a confined `Dir`) carried in an actor
    /// field and used inside a handler. The native binary drains its mailbox after
    /// `main`, exactly as the interpreter's scheduler does.
    #[test]
    fn native_backend_actors() {
        let cases = [
            (
                "actors",
                "examples/actors.witchy",
                "[1] direct message\n[2] another direct\n[3] relayed message\n",
            ),
            ("files", "examples/files.witchy", "hello from a sandboxed Dir capability\n"),
        ];
        for (tag, file, expected) in cases {
            let rust = crate::emit_rust_file(file).expect("transpile actor program");
            assert!(
                rust.contains("fn w_run_actors") && rust.contains("WQUEUE"),
                "{tag}: the actor runtime (mailbox + drain) should be emitted"
            );
            // The interpreter is the source of truth; native must match it.
            let (linked, _) = crate::link_file(file).expect("link");
            let interp = interpreter::run_module(linked, ".", Vec::new())
                .expect("interp run")
                .join("\n");
            assert_eq!(format!("{interp}\n"), expected, "{tag}: interp output drifted");
            let pid = std::process::id();
            let dir = std::env::temp_dir().join(format!("witchy_actor_{tag}_{pid}"));
            let _ = std::fs::create_dir_all(&dir);
            let rs = dir.join("prog.rs");
            let bin = dir.join("prog_bin");
            std::fs::write(&rs, &rust).unwrap();
            if let Ok(st) = std::process::Command::new("rustc")
                .args(["-O", "-C", "overflow-checks=off", "--edition", "2021"])
                .arg(&rs)
                .arg("-o")
                .arg(&bin)
                .status()
            {
                assert!(st.success(), "{tag}: rustc should compile the actor program");
                // files.witchy's root `Dir` is the cwd; run from the crate root so
                // its `examples/data/greeting.txt` read resolves.
                let out = std::process::Command::new(&bin)
                    .current_dir(env!("CARGO_MANIFEST_DIR"))
                    .output()
                    .expect("run");
                assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "{tag}: native output");
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// Security: an actor's capability confinement holds in a *compiled* binary,
    /// through the actor boundary. A `Reader` actor is handed a `Dir` attenuated to
    /// `<base>/data` (via `subdir`), carried in an actor field, and used inside a
    /// message handler. A granted file reads fine; a path-traversal message
    /// (`../secret.txt`) is denied by the Dir capability — the binary exits non-zero
    /// and the secret one directory up never leaks. The interpreter agrees (reads
    /// the granted file, errors on the escape), so the guarantee is identical
    /// whether interpreted or compiled — something Go's runtime cannot express.
    #[test]
    fn native_backend_actor_capability_confinement() {
        let pid = std::process::id();
        let base = std::env::temp_dir().join(format!("witchy_actorcap_{pid}"));
        let _ = std::fs::create_dir_all(base.join("data"));
        std::fs::write(base.join("data/greeting.txt"), "hello from a confined Dir").unwrap();
        std::fs::write(base.join("secret.txt"), "TOP SECRET").unwrap();
        let src = base.join("reader.witchy");
        std::fs::write(
            &src,
            "actor Reader:\n    console: Console\n    dir: Dir\n\nimpl Reader:\n    on Read(name: String):\n        print(console, read(dir, name))\n\nfn main(console: Console, args: List(String), root: Dir):\n    let data = subdir(root, \"data\")\n    let r = spawn Reader(console, data)\n    send(r, Read(at(args, 0)))\n",
        )
        .unwrap();
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains("fn w_run_actors"), "actor runtime should be emitted");
        assert!(rust.contains("escapes the Dir capability"), "Dir confinement helper expected");

        // Interpreter (source of truth): reads the granted file, errors on escape.
        let (linked, _) = crate::link_file(src.to_str().unwrap()).expect("link");
        let interp_ok =
            interpreter::run_module_args(linked, &base, Vec::new(), vec!["greeting.txt".into()])
                .expect("interp granted read")
                .join("\n");
        assert_eq!(interp_ok, "hello from a confined Dir");
        let (linked2, _) = crate::link_file(src.to_str().unwrap()).expect("link");
        assert!(
            interpreter::run_module_args(linked2, &base, Vec::new(), vec!["../secret.txt".into()])
                .is_err(),
            "interpreter must deny the traversal escape"
        );

        let rs = base.join("reader.rs");
        let bin = base.join("reader_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "-C", "overflow-checks=off", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the actor program");
            // Granted file: served, exit 0, byte-identical to the interpreter.
            let ok = std::process::Command::new(&bin)
                .arg("greeting.txt")
                .current_dir(&base)
                .output()
                .expect("run granted");
            assert!(ok.status.success(), "granted read should succeed");
            assert_eq!(String::from_utf8_lossy(&ok.stdout), "hello from a confined Dir\n");
            // Traversal: denied by the Dir capability — non-zero exit, no leak.
            let esc = std::process::Command::new(&bin)
                .arg("../secret.txt")
                .current_dir(&base)
                .output()
                .expect("run escape");
            assert!(!esc.status.success(), "the traversal escape must be denied (non-zero exit)");
            assert!(
                !String::from_utf8_lossy(&esc.stdout).contains("TOP SECRET"),
                "the secret one directory up must never leak"
            );
            assert!(
                String::from_utf8_lossy(&esc.stderr).contains("escapes the Dir capability"),
                "denial should cite the Dir capability"
            );
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// `update(d, k, default, f)` — the single-lookup dict upsert. The native
    /// backend fuses it to one `d.entry(k).or_insert(default)` with the updater
    /// inlined (no closure allocation) and the key moved (no clone), and produces
    /// the same result as the interpreter: a missing key starts at `default`,
    /// an existing key is mapped through `f`.
    #[test]
    fn native_backend_dict_update() {
        let prog = "fn main(console: Console):\n    var d = dict_new()\n    d = update(d, \"a\", 0, fn(n: Int): n + 1)\n    d = update(d, \"a\", 0, fn(n: Int): n + 1)\n    d = update(d, \"b\", 10, fn(n: Int): n + 5)\n    print(console, int_to_string(get_or(d, \"a\", -1)))\n    print(console, int_to_string(get_or(d, \"b\", -1)))\n    print(console, int_to_string(size(d)))\n";
        // Interpreter is the source of truth.
        assert_eq!(interpreter::run(prog).expect("interp").join("\n"), "2\n15\n2");
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_upd_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(&src, prog).unwrap();
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        // Fused to a single entry() upsert — not the two-lookup get_or+insert idiom.
        assert!(rust.contains(".entry("), "update should fuse to entry(): {rust}");
        assert!(!rust.contains("get_or"), "update should not lower to get_or");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "-C", "overflow-checks=off", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the dict-update program");
            let out = std::process::Command::new(&bin).output().expect("run");
            assert_eq!(String::from_utf8_lossy(&out.stdout), "2\n15\n2\n");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// F-strings (and `<>` chains) flatten to a SINGLE `format!`, not one nested
    /// `format!("{}{}", ..)` per `<>`. String literals are baked into the format
    /// string (with `{`/`}` escaped), so `f"a{n}b{n}c"` becomes `format!("a{}b{}c",
    /// ..)` — one allocation, not five. Output is byte-identical to the interpreter,
    /// including literal braces, quotes, and empty pieces.
    #[test]
    fn native_backend_fstring_flattening() {
        let prog = "fn main(console: Console):\n    let n = 42\n    let s = \"x\"\n    print(console, f\"a{n}b{n}c\")\n    print(console, \"lit{brace}\" <> int_to_string(n))\n    print(console, f\"q\\\"{s}\\\"\")\n    print(console, s <> \"\")\n";
        assert_eq!(
            interpreter::run(prog).expect("interp"),
            vec!["a42b42c", "lit{brace}42", "q\"x\"", "x"]
        );
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_fstr_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(&src, prog).unwrap();
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        // One flattened format! — not nested format!("{}{}", format!(..), ..).
        assert!(rust.contains("format!(\"a{}b{}c\""), "f-string should flatten: {rust}");
        assert!(rust.contains("lit{{brace}}"), "literal braces must be escaped: {rust}");
        // Interpolated values format directly through the WDisplay wrapper (writing
        // into the result buffer), not via an intermediate `w_show()` String.
        assert!(rust.contains("WDisplay(&"), "interpolation should use WDisplay: {rust}");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "-C", "overflow-checks=off", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the f-string program");
            let out = std::process::Command::new(&bin).output().expect("run");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "a42b42c\nlit{brace}42\nq\"x\"\nx\n"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `-> Nil` (unit) return type, and an enum recursive through a collection
    /// (`JsonArray(List(Json))`) registered before its fields are computed.
    #[test]
    fn native_backend_nil_and_recursive_enum() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_nil_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "type Tree:\n    Leaf\n    Branch(List(Tree))\n\nfn count(t: Tree) -> Int:\n    match t:\n        Leaf -> 1\n        Branch(kids) -> length(kids)\n\nfn greet(console: Console) -> Nil:\n    print(console, \"hi\")\n\nfn main(console: Console):\n    greet(console)\n    print(console, int_to_string(count(Branch([Leaf, Leaf, Leaf]))))\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains("enum Tree"), "recursive-through-collection enum should register");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the Nil/recursive-enum program");
            let out = std::process::Command::new(&bin).output().expect("run");
            assert_eq!(String::from_utf8_lossy(&out.stdout), "hi\n3\n");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Host primitives (compiler/crypto introspection) error on native rather than
    /// silently transpiling their stub witchy body.
    #[test]
    fn native_backend_host_primitive_errors() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_host_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "import compiler\n\nfn main(console: Console):\n    print(console, compiler.footprint(\"fn main(): 0\"))\n",
        )
        .expect("write src");
        let err = crate::emit_rust_file(src.to_str().unwrap()).unwrap_err();
        assert!(err.contains("host primitive"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `while let PAT = scrut: body` lowers to `loop { match scrut { PAT => body,
    /// _ => break } }`, re-evaluating the scrutinee each iteration.
    #[test]
    fn native_backend_while_let() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_wl_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "fn uncons(xs: List(Int)) -> Option((Int, List(Int))):\n    match xs:\n        [] -> None\n        [head, ..tail] -> Some((head, tail))\n\nfn main(console: Console):\n    var stack = [1, 2, 3, 4]\n    var sum = 0\n    while let Some((top, rest)) = uncons(stack):\n        sum = sum + top\n        stack = rest\n    print(console, int_to_string(sum))\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains("loop { match"), "expected while-let to lower to loop+match");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the while-let program");
            let out = std::process::Command::new(&bin).output().expect("run");
            assert_eq!(String::from_utf8_lossy(&out.stdout), "10\n");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// String patterns in `match` lower to an if-else chain comparing literals;
    /// a variable arm binds the owned `String`.
    #[test]
    fn native_backend_string_patterns() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_sp_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "fn val(s: String) -> Int:\n    match s:\n        \"I\" -> 1\n        \"V\" -> 5\n        \"X\" -> 10\n        _ -> 0\n\nfn main(console: Console):\n    print(console, int_to_string(val(\"V\")))\n    print(console, int_to_string(val(\"X\")))\n    print(console, int_to_string(val(\"abcd\")))\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains("__m == \"V\""), "expected literal comparison");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the string-pattern program");
            let out = std::process::Command::new(&bin).output().expect("run");
            assert_eq!(String::from_utf8_lossy(&out.stdout), "5\n10\n0\n");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// List patterns in `match` — `[]`, `[x]`, `[a, b]`, `[head, ..tail]` —
    /// lower to an if-else chain on length, matching the interpreter top-to-bottom.
    #[test]
    fn native_backend_list_patterns() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_lp_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "fn describe(xs: List(Int)) -> Int:\n    match xs:\n        [] -> 0\n        [x] -> x\n        [a, b] -> a + b\n        [head, ..tail] -> head + length(tail)\n\nfn main(console: Console):\n    print(console, int_to_string(describe([])))\n    print(console, int_to_string(describe([7])))\n    print(console, int_to_string(describe([3, 4])))\n    print(console, int_to_string(describe([10, 1, 2, 3])))\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains("__m.len()"), "expected an if-else-on-length lowering");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the list-pattern program");
            let out = std::process::Command::new(&bin).output().expect("run");
            assert_eq!(String::from_utf8_lossy(&out.stdout), "0\n7\n7\n13\n");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A capability narrowing `e as T` is identity at runtime, so it lowers to
    /// the inner expression (`examples/capability_rights.witchy` exercises it).
    #[test]
    fn native_backend_capability_narrowing_as() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_as_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "fn main(dir: Dir, console: Console):\n    let ro = dir as Dir[Read]\n    print(console, \"ok\")\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        // `dir as Dir[Read]` becomes just `dir`, then bound — no cast emitted.
        assert!(rust.contains("let ro = dir"), "expected `as` to lower to the inner expr: {rust}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Escaping closures: a function may return a closure capturing its params
    /// (`make_adder`), each capture independent; and a captured non-Copy binding
    /// stays usable after the closure (cloned into the `move`, not moved out).
    #[test]
    fn native_backend_escaping_closures() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_clos_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "import list\n\nfn make_adder(n: Int) -> fn(Int) -> Int:\n    fn(x: Int): x + n\n\nfn main(console: Console):\n    let add5 = make_adder(5)\n    let add10 = make_adder(10)\n    print(console, int_to_string(add5(100)))\n    print(console, int_to_string(add10(100)))\n    let prefix = \"Mr. \"\n    let g = list.map([\"A\", \"B\"], fn(n: String): f\"{prefix}{n}\")\n    print(console, f\"{g}\")\n    print(console, prefix)\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains("move |"), "expected move closures");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the escaping-closure program");
            let out = std::process::Command::new(&bin).output().expect("run");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "105\n110\n[Mr. A, Mr. B]\nMr. \n"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Security: a native binary whose `Net` capability allows only one address
    /// must reject `connect` to every other address — at the gate, before any
    /// socket is opened. The allowed address passes the gate (and fails later
    /// only because nothing is listening, a different error).
    #[test]
    fn native_backend_net_confinement_holds() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_netsec_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "fn main(net: Net, args: List(String), console: Console):\n    let s = connect(net, at(args, 0))\n    print(console, \"connected\")\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        else {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        assert!(st.success(), "the connect program should compile");
        // Only 127.0.0.1:9 is permitted; every other target must be denied.
        let denied = ["127.0.0.1:8", "127.0.0.1:80", "10.0.0.1:22", "0.0.0.0:1"];
        for target in denied {
            let out = std::process::Command::new(&bin)
                .arg(target)
                .env("WITCHY_NET", "127.0.0.1:9")
                .output()
                .expect("run");
            let err = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                err.contains("not permitted by this Net capability"),
                "target {target:?} should be denied at the gate"
            );
            assert!(!stdout.contains("connected"), "target {target:?} must not connect");
        }
        // The permitted address passes the gate (then fails to connect: nothing
        // is listening — but crucially NOT with a capability denial).
        let allowed = std::process::Command::new(&bin)
            .arg("127.0.0.1:9")
            .env("WITCHY_NET", "127.0.0.1:9")
            .output()
            .expect("run");
        assert!(
            !String::from_utf8_lossy(&allowed.stderr).contains("not permitted"),
            "the allowed address must pass the capability gate"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Security: a native binary holding a `Dir` capability rooted at a directory
    /// must NEVER read a file outside that directory, for any adversarial path —
    /// the capability is a hard subtree sandbox. Plants a secret as a sibling of
    /// the root and asserts it never leaks, across many escape attempts.
    #[test]
    fn native_backend_dir_confinement_holds() {
        let pid = std::process::id();
        let base = std::env::temp_dir().join(format!("witchy_sandbox_{pid}"));
        let root = base.join("root");
        let _ = std::fs::create_dir_all(&root);
        std::fs::write(root.join("ok.txt"), "in-root-ok").expect("write ok");
        // The secret lives OUTSIDE the root (a sibling and a parent).
        std::fs::write(base.join("secret.txt"), "TOP-SECRET").expect("write secret");
        let src = root.join("prog.witchy");
        std::fs::write(
            &src,
            "fn main(dir: Dir, args: List(String), console: Console):\n    print(console, read(dir, at(args, 0)))\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        let rs = root.join("prog.rs");
        let bin = root.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        else {
            let _ = std::fs::remove_dir_all(&base);
            return; // no rustc available
        };
        assert!(st.success(), "the sandbox program should compile");
        // Every one of these must be rejected (panic) — none may read the secret.
        let escapes = [
            "../secret.txt",
            "../../secret.txt",
            "ok.txt/../../secret.txt",
            "./../secret.txt",
            "a/b/../../../secret.txt",
            "/etc/hosts",
            "..",
            "../",
            "....//secret.txt",
        ];
        for path in escapes {
            let out = std::process::Command::new(&bin)
                .arg(path)
                .current_dir(&root)
                .output()
                .expect("run");
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                !stdout.contains("TOP-SECRET"),
                "sandbox escape: path {path:?} leaked the secret"
            );
        }
        // The in-root file is readable (the sandbox isn't broken, just confined).
        let ok = std::process::Command::new(&bin)
            .arg("ok.txt")
            .current_dir(&root)
            .output()
            .expect("run");
        assert_eq!(String::from_utf8_lossy(&ok.stdout), "in-root-ok\n");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The Clock capability: `now(clock)` returns ms since the epoch. Tested by a
    /// deterministic bound (the absolute value is wall-clock, so not diff-able).
    #[test]
    fn native_backend_clock_now() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_clock_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "fn main(clock: Clock, console: Console):\n    print(console, f\"{now(clock) > 1000000000000}\")\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains("SystemTime::now()"), "expected the clock read");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the clock program");
            let out = std::process::Command::new(&bin).output().expect("run");
            assert_eq!(String::from_utf8_lossy(&out.stdout), "true\n");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The rest of the Dir capability — `exists`, `make_dir`, `list` (sorted) —
    /// confined like read/write, matching the interpreter.
    #[test]
    fn native_backend_dir_exists_make_list() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_dirops_{pid}"));
        let data = dir.join("data");
        let _ = std::fs::create_dir_all(&data);
        std::fs::write(data.join("a.txt"), "x").expect("write data");
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "fn main(dir: Dir, console: Console):\n    print(console, f\"{exists(dir, \"a.txt\")}\")\n    print(console, f\"{exists(dir, \"missing.txt\")}\")\n    print(console, f\"{exists(dir, \"../escape\")}\")\n    make_dir(dir, \"made\")\n    print(console, f\"{list(dir)}\")\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains("w_dir_exists("), "expected exists");
        assert!(rust.contains("w_dir_make("), "expected make_dir");
        assert!(rust.contains("w_dir_list("), "expected list");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the Dir-ops program");
            // Run with the data dir as cwd, so the Dir capability sees only it.
            let out = std::process::Command::new(&bin)
                .current_dir(&data)
                .output()
                .expect("run");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "true\nfalse\nfalse\n[a.txt, made]\n"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The `sqrt` float primitive maps to Rust's `f64::sqrt`, matching the
    /// interpreter and wasm.
    #[test]
    fn native_backend_sqrt() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_sqrt_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "fn main(console: Console):\n    print(console, to_string(sqrt(16.0)))\n    print(console, int_to_string(float_to_int(sqrt(144.0))))\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains(".sqrt()"), "expected f64::sqrt");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the sqrt program");
            let out = std::process::Command::new(&bin).output().expect("run");
            assert_eq!(String::from_utf8_lossy(&out.stdout), "4\n12\n");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Showing user records/enums (`Point(3, 4)`, `Circle(7)`, `Dot`) matches the
    /// interpreter, including a list of them — gated by a showability fixpoint.
    #[test]
    fn native_backend_record_enum_display() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_rshow_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "type Point:\n    Point(Int, Int)\n\ntype Shape:\n    Circle(Int)\n    Rect(Int, Int)\n    Dot\n\nfn main(console: Console):\n    print(console, f\"{Point(3, 4)}\")\n    print(console, f\"{[Point(1, 2), Point(5, 6)]}\")\n    print(console, f\"{Circle(7)}\")\n    print(console, f\"{Rect(2, 3)}\")\n    print(console, f\"{Dot}\")\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains("WShow for Point"), "expected a record WShow impl");
        assert!(rust.contains("WShow for Shape"), "expected an enum WShow impl");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the record/enum-display program");
            let out = std::process::Command::new(&bin).output().expect("run");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "Point(3, 4)\n[Point(1, 2), Point(5, 6)]\nCircle(7)\nRect(2, 3)\nDot\n"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A user type with a non-showable field (a dict) gets no `WShow` impl, but
    /// still compiles and runs — the showability fixpoint must not break it.
    #[test]
    fn native_backend_unshowable_record_still_compiles() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_unshow_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "type Cache:\n    Cache(Dict(String, Int), Int)\n\nfn size_of(c: Cache) -> Int:\n    match c:\n        Cache(d, n) -> n\n\nfn main(console: Console):\n    var d = dict_new()\n    d = insert(d, \"a\", 1)\n    print(console, int_to_string(size_of(Cache(d, 42))))\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(!rust.contains("WShow for Cache"), "a dict-holding record must not get WShow");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile despite the unshowable field");
            let out = std::process::Command::new(&bin).output().expect("run");
            assert_eq!(String::from_utf8_lossy(&out.stdout), "42\n");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Showing collections in `print`/f-strings (`[a, b]`, `(a, b)`, `Some(x)`)
    /// matches the interpreter's `Display` exactly, including nesting.
    #[test]
    fn native_backend_collection_display() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_show_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "fn main(console: Console):\n    print(console, f\"{[1, 2, 3]}\")\n    print(console, f\"{[\"a\", \"b\"]}\")\n    print(console, f\"nested: {[[1, 2], [3]]}\")\n    print(console, f\"tuple: {(1, 99, true)}\")\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains("trait WShow"), "expected the WShow formatter");
        assert!(rust.contains(".w_show()"), "expected w_show calls");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the collection-display program");
            let out = std::process::Command::new(&bin).output().expect("run");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "[1, 2, 3]\n[a, b]\nnested: [[1, 2], [3]]\ntuple: (1, 99, true)\n"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A named top-level function passed as a first-class value (`map(xs, dbl)`,
    /// `filter(xs, pred)`) — resolved to its Rust `w_*` item and kept reachable.
    #[test]
    fn native_backend_named_function_value() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_fnval_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "import list\n\nfn double(x: Int) -> Int:\n    x * 2\n\nfn is_big(x: Int) -> Bool:\n    x > 3\n\nfn main(console: Console):\n    let xs = [1, 2, 3, 4]\n    print(console, int_to_string(list.sum(list.map(xs, double))))\n    print(console, int_to_string(length(list.filter(xs, is_big))))\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        assert!(rust.contains("Rc::new(w_"), "expected a named fn passed as a value (`Rc::new(w_*)`)");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the fn-value program");
            let out = std::process::Command::new(&bin).output().expect("run");
            assert_eq!(String::from_utf8_lossy(&out.stdout), "20\n1\n");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Value semantics for a reused binding used as a *value* (not just a call
    /// arg): `let ys = xs` and a branch result keep the source usable afterward
    /// (cloned, not moved). Regression guard for last-use move elision.
    #[test]
    fn native_backend_value_reuse() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("witchy_vreuse_{pid}"));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("prog.witchy");
        std::fs::write(
            &src,
            "fn pick(c: Bool, s: String) -> String:\n    let head = if c:\n        s\n    else:\n        substring(s, 0, 1)\n    f\"{head}|{s}\"\n\nfn main(console: Console):\n    let xs = [1, 2, 3]\n    let ys = xs\n    print(console, f\"{length(xs)}+{length(ys)}\")\n    print(console, pick(true, \"abc\"))\n    print(console, pick(false, \"abc\"))\n",
        )
        .expect("write src");
        let rust = crate::emit_rust_file(src.to_str().unwrap()).expect("transpile");
        let rs = dir.join("prog.rs");
        let bin = dir.join("prog_bin");
        std::fs::write(&rs, &rust).unwrap();
        if let Ok(st) = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status()
        {
            assert!(st.success(), "rustc should compile the value-reuse program");
            let out = std::process::Command::new(&bin).output().expect("run");
            assert_eq!(String::from_utf8_lossy(&out.stdout), "3+3\nabc|abc\na|abc\n");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Python-style f-strings: `f"...{expr}..."` interpolates (with `{{`/`}}` for
    /// literal braces), desugaring to `to_string` + concat — same result on both
    /// backends.
    #[test]
    fn f_strings_interpolate() {
        let src = "fn main(console: Console):\n    let name = \"world\"\n    let n = 6\n    print(console, f\"hi {name} #{n * 7}\")\n    print(console, f\"{{braces}}\")\n";
        assert_eq!(interp(src), vec!["hi world #42", "{braces}"]);
        assert_eq!(run_on_wasm(src), vec!["hi world #42", "{braces}"]);
    }

    /// `witchy emit-wat <file>` compiles a program to WebAssembly text — the same
    /// module `sandbox` runs — for inspecting the generated code.
    #[test]
    fn emit_wat_returns_the_compiled_module() {
        let path = std::env::temp_dir().join(format!("witchy_emit_wat_{}.witchy", std::process::id()));
        std::fs::write(
            &path,
            "fn fib(n: Int) -> Int:\n    if n < 2:\n        n\n    else:\n        fib(n - 1) + fib(n - 2)\nfn main(console: Console):\n    print(console, int_to_string(fib(10)))\n",
        )
        .expect("write temp source");
        let wat = crate::emit_wat_file(path.to_str().unwrap()).expect("emit-wat");
        let _ = std::fs::remove_file(&path);
        assert!(wat.starts_with("(module"), "expected a wasm module, got: {}", &wat[..wat.len().min(40)]);
        // The fib function is emitted, module-qualified by the file stem.
        assert!(wat.contains(".fib (param $n i64)"), "expected the fib function in the WAT");
    }

    /// The native backend transpiles the compute subset to Rust; transpile a
    /// recursive-fib program and (when rustc is available) compile + run it,
    /// confirming the native binary matches the interpreter (fib(20) = 6765).
    #[test]
    fn native_backend_transpiles_and_runs() {
        let pid = std::process::id();
        let src_path = std::env::temp_dir().join(format!("witchy_native_{pid}.witchy"));
        std::fs::write(
            &src_path,
            "fn fib(n: Int) -> Int:\n    if n < 2:\n        n\n    else:\n        fib(n - 1) + fib(n - 2)\nfn main(console: Console):\n    print(console, int_to_string(fib(20)))\n",
        )
        .expect("write source");
        let rust = crate::emit_rust_file(src_path.to_str().unwrap()).expect("transpile");
        let _ = std::fs::remove_file(&src_path);
        assert!(rust.contains("fn main()"), "expected a native main");
        assert!(rust.contains("n: i64) -> i64"), "expected i64 typing of fib");
        // Full path: compile the generated Rust with rustc and run it.
        let rs = std::env::temp_dir().join(format!("witchy_native_{pid}.rs"));
        let bin = std::env::temp_dir().join(format!("witchy_native_{pid}_bin"));
        std::fs::write(&rs, &rust).expect("write rust");
        let compiled = std::process::Command::new("rustc")
            .args(["-O", "--edition", "2021"])
            .arg(&rs)
            .arg("-o")
            .arg(&bin)
            .status();
        if let Ok(s) = compiled {
            assert!(s.success(), "rustc should compile the generated Rust");
            let out = std::process::Command::new(&bin).output().expect("run native");
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "6765");
            let _ = std::fs::remove_file(&bin);
        }
        let _ = std::fs::remove_file(&rs);
    }

    /// The native backend transpiles tuples (multi-value returns) and tuple
    /// destructuring directly to Rust tuples.
    #[test]
    fn native_backend_supports_tuples() {
        let p = std::env::temp_dir()
            .join(format!("witchy_native_tup_{}.witchy", std::process::id()));
        std::fs::write(
            &p,
            "fn divmod(a: Int, b: Int) -> (Int, Int):\n    (a / b, a % b)\nfn main(console: Console):\n    let (q, r) = divmod(17, 5)\n    print(console, int_to_string(q + r))\n",
        )
        .expect("write source");
        let rust = crate::emit_rust_file(p.to_str().unwrap()).expect("transpile");
        let _ = std::fs::remove_file(&p);
        assert!(rust.contains("-> (i64, i64)"), "expected a tuple return type");
        assert!(rust.contains("let (q, r) ="), "expected tuple destructuring");
    }

    /// The native backend transpiles `match` on scalar patterns, with guards, to
    /// a Rust match expression.
    #[test]
    fn native_backend_supports_match() {
        let p = std::env::temp_dir()
            .join(format!("witchy_native_match_{}.witchy", std::process::id()));
        std::fs::write(
            &p,
            "fn sign(n: Int) -> Int:\n    match n:\n        0 -> 0\n        x if x < 0 -> -1\n        _ -> 1\nfn main(console: Console):\n    print(console, int_to_string(sign(-5)))\n",
        )
        .expect("write source");
        let rust = crate::emit_rust_file(p.to_str().unwrap()).expect("transpile");
        let _ = std::fs::remove_file(&p);
        assert!(rust.contains("match n {"), "expected a Rust match");
        assert!(rust.contains("if (x < 0i64) =>"), "expected a match guard");
    }

    /// The native backend transpiles record types to Rust structs — declaration,
    /// construction, and field access.
    #[test]
    fn native_backend_supports_records() {
        let p = std::env::temp_dir()
            .join(format!("witchy_native_rec_{}.witchy", std::process::id()));
        std::fs::write(
            &p,
            "type Point:\n    x: Int\n    y: Int\nfn main(console: Console):\n    let p = Point(x: 3, y: 4)\n    print(console, int_to_string(p.x + p.y))\n",
        )
        .expect("write source");
        let rust = crate::emit_rust_file(p.to_str().unwrap()).expect("transpile");
        let _ = std::fs::remove_file(&p);
        assert!(rust.contains("struct Point {"), "expected a Rust struct");
        assert!(rust.contains("Point { x:"), "expected struct construction");
        assert!(rust.contains(".x.clone()"), "expected field access");
    }

    /// The native backend transpiles concrete (non-generic) sum types to Rust
    /// enums, with qualified construction and constructor patterns in `match`.
    #[test]
    fn native_backend_supports_enums() {
        let p = std::env::temp_dir()
            .join(format!("witchy_native_enum_{}.witchy", std::process::id()));
        std::fs::write(
            &p,
            "type Shape:\n    Circle(Int)\n    Unit\nfn area(s: Shape) -> Int:\n    match s:\n        Circle(r) -> 3 * r * r\n        Unit -> 0\nfn main(console: Console):\n    print(console, int_to_string(area(Circle(10))))\n",
        )
        .expect("write source");
        let rust = crate::emit_rust_file(p.to_str().unwrap()).expect("transpile");
        let _ = std::fs::remove_file(&p);
        assert!(rust.contains("enum Shape {"), "expected a Rust enum");
        assert!(rust.contains("Shape::Circle(10i64)"), "expected qualified construction");
        assert!(rust.contains("Shape::Circle(r) =>"), "expected a qualified pattern");
    }

    /// Option/Result and the `?` operator map onto Rust's built-in Option/Result.
    #[test]
    fn native_backend_supports_option_result_and_try() {
        let p = std::env::temp_dir()
            .join(format!("witchy_native_opt_{}.witchy", std::process::id()));
        std::fs::write(
            &p,
            "fn safe_div(a: Int, b: Int) -> Option(Int):\n    if b == 0:\n        None\n    else:\n        Some(a / b)\nfn chain(a: Int, b: Int) -> Option(Int):\n    let x = safe_div(a, b)?\n    Some(x + 1)\nfn main(console: Console):\n    match chain(10, 2):\n        Some(v) -> print(console, int_to_string(v))\n        None -> print(console, \"none\")\n",
        )
        .expect("write source");
        let rust = crate::emit_rust_file(p.to_str().unwrap()).expect("transpile");
        let _ = std::fs::remove_file(&p);
        assert!(rust.contains("-> Option<i64>"), "expected Option<i64> typing");
        assert!(rust.contains(")?"), "expected the `?` operator");
        assert!(rust.contains("Some(") && rust.contains("None"), "expected Some/None");
    }

    /// Lists map to `Vec<T>`: literals, `push`/`length`/`at`, and `for`-over-list,
    /// with collection arguments cloned at the call site so value semantics hold
    /// (the caller keeps its list after passing it).
    #[test]
    fn native_backend_supports_lists() {
        let p = std::env::temp_dir()
            .join(format!("witchy_native_list_{}.witchy", std::process::id()));
        std::fs::write(
            &p,
            "fn sum(xs: List(Int)) -> Int:\n    var t = 0\n    for x in xs:\n        t = t + x\n    t\nfn main(console: Console):\n    var acc = []\n    for i in 0..4:\n        acc = push(acc, i * i)\n    print(console, int_to_string(sum(acc)))\n    print(console, int_to_string(length(acc)))\n",
        )
        .expect("write source");
        let rust = crate::emit_rust_file(p.to_str().unwrap()).expect("transpile");
        let _ = std::fs::remove_file(&p);
        assert!(rust.contains("Vec<i64>"), "expected a Vec<i64> list type");
        assert!(rust.contains(".push("), "expected an in-place push");
        assert!(
            rust.contains("(acc).clone()"),
            "expected the list arg cloned at the call so the caller keeps it"
        );
    }

    /// String builtins and numeric conversions map to Rust String/str methods.
    #[test]
    fn native_backend_supports_strings_and_conversions() {
        let p = std::env::temp_dir()
            .join(format!("witchy_native_str_{}.witchy", std::process::id()));
        std::fs::write(
            &p,
            "fn main(console: Console):\n    print(console, substring(\"hello\", 1, 3))\n    print(console, to_upper(\"hi\"))\n    print(console, replace(\"aXa\", \"X\", \"-\"))\n    print(console, int_to_string(float_to_int(int_to_float(7) / int_to_float(2) * 10.0)))\n",
        )
        .expect("write source");
        let rust = crate::emit_rust_file(p.to_str().unwrap()).expect("transpile");
        let _ = std::fs::remove_file(&p);
        assert!(rust.contains(".chars()"), "substring should use chars()");
        assert!(rust.contains(".to_uppercase()"), "to_upper -> to_uppercase");
        assert!(rust.contains(".replace("), "replace");
        assert!(rust.contains("as f64") && rust.contains("as i64"), "numeric conversions");
    }

    /// Dicts map to a fast HashMap (FxHash, non-DoS); insert/get_or/has/size.
    /// Iteration order is unspecified, so this checks order-independent results.
    #[test]
    fn native_backend_supports_dicts() {
        let p = std::env::temp_dir()
            .join(format!("witchy_native_dict_{}.witchy", std::process::id()));
        std::fs::write(
            &p,
            "fn main(console: Console):\n    var d = dict_new()\n    d = insert(d, 1, 10)\n    d = insert(d, 2, 20)\n    print(console, int_to_string(get_or(d, 1, 0) + get_or(d, 2, 0) + get_or(d, 9, -1)))\n    print(console, int_to_string(size(d)))\n",
        )
        .expect("write source");
        let rust = crate::emit_rust_file(p.to_str().unwrap()).expect("transpile");
        let _ = std::fs::remove_file(&p);
        assert!(rust.contains("FxHasher"), "expected the fast-hash dict helper");
        assert!(rust.contains("WMap::default()"), "expected dict_new -> WMap");
        assert!(rust.contains(".insert("), "expected dict insert");
    }

    /// Generic higher-order functions: type variables become Rust generics,
    /// `fn(a)->b` params become `Rc<dyn Fn(..)>`, lambdas become `Rc::new(..)` (with
    /// captures), and a call to a function-valued parameter calls it directly.
    #[test]
    fn native_backend_supports_generics_and_closures() {
        let p = std::env::temp_dir()
            .join(format!("witchy_native_hof_{}.witchy", std::process::id()));
        std::fs::write(
            &p,
            "fn my_map(xs: List(a), f: fn(a) -> b) -> List(b):\n    var acc = []\n    for x in xs:\n        acc = push(acc, f(x))\n    acc\nfn main(console: Console):\n    let bias = 10\n    let ys = my_map([1, 2, 3], fn(n): n + bias)\n    var s = 0\n    for y in ys:\n        s = s + y\n    print(console, int_to_string(s))\n",
        )
        .expect("write source");
        let rust = crate::emit_rust_file(p.to_str().unwrap()).expect("transpile");
        let _ = std::fs::remove_file(&p);
        assert!(rust.contains("<A: Clone + 'static, B: Clone + 'static>"), "expected generic params");
        assert!(rust.contains("Rc<dyn Fn(A) -> B>"), "expected fn-type param");
        assert!(rust.contains("|n|"), "expected a closure for the lambda");
        assert!(rust.contains("(f)("), "expected a direct call of the fn param");
    }

    /// A program using the stdlib higher-order functions (`list.map`/`filter`/
    /// `fold`) transpiles: only the reached generic functions are emitted (so an
    /// imported module's unused, out-of-subset functions don't block it).
    #[test]
    fn native_backend_supports_stdlib_higher_order() {
        // A fresh subdir, so no sibling `list.witchy` shadows the std module.
        let dir = std::env::temp_dir().join(format!("witchy_hof_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("main.witchy");
        std::fs::write(
            &path,
            "import list\nfn main(console: Console):\n    let evens = list.filter([1, 2, 3, 4], fn(n): n % 2 == 0)\n    print(console, int_to_string(list.fold(list.map(evens, fn(n): n * 10), 0, fn(a, x): a + x)))\n",
        )
        .expect("write source");
        let rust = crate::emit_rust_file(path.to_str().unwrap()).expect("transpile");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
        assert!(rust.contains("w_list_map"), "expected stdlib map transpiled");
        assert!(rust.contains("w_list_filter"), "expected stdlib filter transpiled");
        assert!(rust.contains("Rc<dyn Fn"), "expected fn-type params");
    }

    /// Generic user records/enums become generic Rust structs/enums, with type
    /// arguments carried on references (`Pair(a, b)` -> `Pair<A, B>`).
    #[test]
    fn native_backend_supports_generic_user_types() {
        let p = std::env::temp_dir()
            .join(format!("witchy_native_gen_{}.witchy", std::process::id()));
        std::fs::write(
            &p,
            "type Pair:\n    first: a\n    second: b\nfn swap(p: Pair(a, b)) -> Pair(b, a):\n    Pair(first: p.second, second: p.first)\nfn main(console: Console):\n    let q = swap(Pair(first: 7, second: 100))\n    print(console, int_to_string(q.first + q.second))\n",
        )
        .expect("write source");
        let rust = crate::emit_rust_file(p.to_str().unwrap()).expect("transpile");
        let _ = std::fs::remove_file(&p);
        assert!(rust.contains("struct Pair<A: Clone + 'static, B: Clone + 'static>"), "expected a generic struct");
        assert!(rust.contains("-> Pair<B, A>"), "expected type args on the return");
    }

    /// Record update (`Point(x: 5, ..p)`) clones the base and reassigns the named
    /// fields, leaving the original untouched.
    #[test]
    fn native_backend_supports_record_update() {
        let p = std::env::temp_dir()
            .join(format!("witchy_native_upd_{}.witchy", std::process::id()));
        std::fs::write(
            &p,
            "type Point:\n    x: Int\n    y: Int\nfn main(console: Console):\n    let a = Point(x: 1, y: 2)\n    let b = Point(x: 10, ..a)\n    print(console, int_to_string(a.x + b.x + b.y))\n",
        )
        .expect("write source");
        let rust = crate::emit_rust_file(p.to_str().unwrap()).expect("transpile");
        let _ = std::fs::remove_file(&p);
        assert!(rust.contains("let mut __r = (a).clone();"), "expected clone-then-update");
        assert!(rust.contains("__r.x = 10i64;"), "expected the updated field");
    }

    /// Recursive enums (e.g. a binary tree): recursive fields are `Box`ed, boxed
    /// at construction and moved out of the box at `match`.
    #[test]
    fn native_backend_supports_recursive_enums() {
        let p = std::env::temp_dir()
            .join(format!("witchy_native_tree_{}.witchy", std::process::id()));
        std::fs::write(
            &p,
            "type Tree:\n    Leaf(Int)\n    Node(Tree, Tree)\nfn sum(t: Tree) -> Int:\n    match t:\n        Leaf(n) -> n\n        Node(l, r) -> sum(l) + sum(r)\nfn main(console: Console):\n    print(console, int_to_string(sum(Node(Leaf(1), Node(Leaf(2), Leaf(3))))))\n",
        )
        .expect("write source");
        let rust = crate::emit_rust_file(p.to_str().unwrap()).expect("transpile");
        let _ = std::fs::remove_file(&p);
        assert!(rust.contains("Node(Box<Tree>, Box<Tree>)"), "expected boxed recursive fields");
        assert!(rust.contains("Box::new("), "expected Box::new at construction");
        assert!(rust.contains("let l = *l;"), "expected move-out-of-box at match");
    }

    /// `for x in a..b` is a counting loop on both backends — never a materialized
    /// list — with faithful `break`/`continue`, inclusive (`..=`), empty, and
    /// nested behavior. The 100_000-iteration loop proves nothing is allocated:
    /// `run_on_wasm` caps memory at 4 pages, so a materialized range would trap.
    #[test]
    fn range_for_loops_match_on_both_backends() {
        let src = r#"fn main(console: Console):
    var a = 0
    for i in 0..5:
        a = a + i
    print(console, int_to_string(a))
    var b = 0
    for i in 1..=5:
        b = b + i
    print(console, int_to_string(b))
    var c = 0
    for i in 0..100:
        if i == 10:
            break
        c = c + i
    print(console, int_to_string(c))
    var d = 0
    for i in 0..10:
        if i % 2 == 0:
            continue
        d = d + i
    print(console, int_to_string(d))
    var e = 0
    for i in 5..5:
        e = e + 1
    for i in 5..2:
        e = e + 1
    print(console, int_to_string(e))
    var f = 0
    for i in 0..3:
        for j in 0..3:
            f = f + i * j
    print(console, int_to_string(f))
    var g = 0
    for i in 0..100000:
        g = g + 1
    print(console, int_to_string(g))
"#;
        let expected = vec!["10", "15", "45", "25", "0", "9", "100000"];
        assert_eq!(interp(src), expected);
        assert_eq!(run_on_wasm(src), expected);
    }

    /// Property tests: a `for` over a random range must compute exactly the same
    /// result as a Rust reference range, on BOTH backends (so they also agree
    /// with each other) — across sign, inclusive/exclusive, empty, and `continue`.
    mod range_for_properties {
        use super::{interp, run_on_wasm};
        use proptest::prelude::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(96))]

            #[test]
            fn sum_matches_reference(lo in -300i64..300, len in 0i64..600, inclusive in any::<bool>()) {
                let hi = lo + len;
                let op = if inclusive { "..=" } else { ".." };
                let src = format!(
                    "fn main(console: Console):\n    var s = 0\n    for i in {lo}{op}{hi}:\n        s = s + i\n    print(console, int_to_string(s))\n"
                );
                let reference: i64 = if inclusive { (lo..=hi).sum() } else { (lo..hi).sum() };
                let want = vec![reference.to_string()];
                prop_assert_eq!(interp(&src), want.clone());
                prop_assert_eq!(run_on_wasm(&src), want);
            }

            #[test]
            fn continue_skipping_odds_matches_reference(lo in -100i64..100, len in 0i64..300) {
                let hi = lo + len;
                let src = format!(
                    "fn main(console: Console):\n    var s = 0\n    for i in {lo}..{hi}:\n        if i % 2 != 0:\n            continue\n        s = s + i\n    print(console, int_to_string(s))\n"
                );
                let reference: i64 = (lo..hi).filter(|x| x % 2 == 0).sum();
                let want = vec![reference.to_string()];
                prop_assert_eq!(interp(&src), want.clone());
                prop_assert_eq!(run_on_wasm(&src), want);
            }
        }
    }

    /// `crypto.ed25519_verify` — a native intrinsic of the `crypto` module — is a
    /// total signature check: it accepts a genuine signature and rejects a
    /// tampered message and malformed input.
    #[test]
    fn crypto_ed25519_verify_checks_signatures() {
        use ed25519_dalek::{Signer, SigningKey};
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let hex = |bs: &[u8]| -> String { bs.iter().map(|b| format!("{b:02x}")).collect() };
        let pk = hex(sk.verifying_key().as_bytes());
        let msg = "release: acme/widget@1.0.0";
        let sig = hex(&sk.sign(msg.as_bytes()).to_bytes());

        let prog = |pubk: &str, m: &str, s: &str| {
            format!(
                "import crypto\nfn main(console: Console):\n    print(console, if crypto.ed25519_verify(\"{pubk}\", \"{m}\", \"{s}\"): \"ok\" else: \"bad\")\n"
            )
        };
        assert_eq!(link_run(&prog(&pk, msg, &sig)), vec!["ok"], "valid signature must verify");
        assert_eq!(
            link_run(&prog(&pk, "release: acme/widget@1.0.1", &sig)),
            vec!["bad"],
            "tampered message must fail"
        );
        assert_eq!(link_run(&prog(&pk, msg, "00")), vec!["bad"], "malformed sig must fail, not panic");
    }

    /// `crypto.ed25519_verify` runs in the *compiled WASM backend* too — bridged
    /// into the sandbox as a host import that calls the same `native` registry
    /// the interpreter uses, so the two tiers agree. (The native module runs at
    /// full Rust speed on the host; the sandbox only sees this one pure import.)
    #[test]
    fn crypto_ed25519_verify_runs_in_the_wasm_backend() {
        use ed25519_dalek::{Signer, SigningKey};
        let sk = SigningKey::from_bytes(&[9u8; 32]);
        let hex = |bs: &[u8]| -> String { bs.iter().map(|b| format!("{b:02x}")).collect() };
        let pk = hex(sk.verifying_key().as_bytes());
        let msg = "wasm-signed";
        let sig = hex(&sk.sign(msg.as_bytes()).to_bytes());
        let prog = |m: &str| {
            format!(
                "import crypto\nfn main(console: Console):\n    print(console, if crypto.ed25519_verify(\"{pk}\", \"{m}\", \"{sig}\"): \"ok\" else: \"bad\")\n"
            )
        };
        let wasm = |src: &str| -> Vec<String> {
            let module = parser::parse_module(src).expect("parse");
            let linked = crate::linker::link(vec![("main".into(), module)], "main").expect("link");
            typeck::check(&linked).expect("typecheck");
            let wat = codegen::compile_module(&linked).expect("compile");
            crate::run_wat_capture(&wat).expect("wasm run")
        };
        // Genuine signature verifies in both backends; a tampered message fails
        // in both — the WASM host import and the interpreter agree.
        assert_eq!(wasm(&prog(msg)), vec!["ok"]);
        assert_eq!(link_run(&prog(msg)), vec!["ok"]);
        assert_eq!(wasm(&prog("tampered")), vec!["bad"]);
        assert_eq!(link_run(&prog("tampered")), vec!["bad"]);
    }

    /// Signing round-trips entirely in witchy: a host-granted `SigningKey`
    /// capability signs a message (`crypto.sign`), and `crypto.ed25519_verify`
    /// against the key's public half (`crypto.public_key`) accepts it. Without a
    /// granted key, a `SigningKey` parameter is refused, and the capability
    /// surfaces in the footprint.
    #[test]
    fn crypto_signing_round_trips_in_witchy() {
        let src = "import crypto\nfn main(console: Console, signer: SigningKey):\n    let msg = \"sign me\"\n    let sig = crypto.sign(signer, msg)\n    print(console, if crypto.ed25519_verify(crypto.public_key(signer), msg, sig): \"verified\" else: \"FAILED\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::linker::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let out = interpreter::run_module_signed(linked, ".", Vec::new(), Vec::new(), Some([7u8; 32]))
            .expect("run");
        assert_eq!(out, vec!["verified"]);

        // A `SigningKey` parameter without a host-granted key is refused.
        let m2 = parser::parse_module("fn main(console: Console, s: SigningKey):\n    print(console, \"x\")\n").expect("parse");
        let l2 = crate::linker::link(vec![("main".into(), m2)], "main").expect("link");
        assert!(interpreter::run_module_signed(l2, ".", Vec::new(), Vec::new(), None).is_err());

        // The signing authority surfaces in the capability footprint.
        let fp = crate::capabilities::analyze(
            &parser::parse_module("fn main(console: Console, s: SigningKey):\n    print(console, \"x\")\n").expect("parse"),
        );
        assert!(fp.total.contains_key("SigningKey"), "SigningKey should appear in the footprint");
    }

    /// `compiler.footprint` exposes witchy's own capability analyzer to witchy
    /// programs (the heart of a self-hosted package manager): it returns the
    /// rights-precise footprint as JSON, which composes with `std/json`.
    #[test]
    fn compiler_footprint_exposes_the_analyzer() {
        // The rights-precise footprint comes back as JSON.
        let out = link_run(
            "import compiler\nfn main(console: Console):\n    print(console, compiler.footprint(\"pub fn load(d: Dir[Read]) -> String:\\n    read(d, \\\"x\\\")\\n\"))\n",
        );
        assert!(out[0].contains("\"total\":[\"Dir[Read]\"]"), "total wrong: {}", out[0]);
        assert!(out[0].contains("\"name\":\"load\""), "entry missing: {}", out[0]);
        // The output is valid JSON — it round-trips through `std/json`.
        let composed = link_run(
            "import compiler\nimport json\nfn main(console: Console):\n    match json.decode(compiler.footprint(\"pub fn serve(n: Net) -> Int:\\n    0\\n\")):\n        Ok(doc) -> print(console, \"valid\")\n        Err(e) -> print(console, \"invalid: \" <> e)\n",
        );
        assert_eq!(composed, vec!["valid"]);
        // Malformed source degrades to an error object, not a crash.
        let bad = link_run(
            "import compiler\nfn main(console: Console):\n    print(console, compiler.footprint(\"fn oops(\"))\n",
        );
        assert!(bad[0].contains("\"error\""), "expected an error object: {}", bad[0]);
    }

    /// `compiler.diff` is the rights-precise block-on-widening gate (the package
    /// manager's core safety check), exposed to witchy as JSON.
    #[test]
    fn compiler_diff_is_the_widening_gate() {
        let diff = |old: &str, new: &str| -> String {
            link_run(&format!(
                "import compiler\nfn main(console: Console):\n    print(console, compiler.diff(\"{old}\", \"{new}\"))\n"
            ))
            .remove(0)
        };
        // A connect-only client that gains `Listen` is a widening (the gate blocks).
        let widen = diff(
            "pub fn f(n: Net[Connect]) -> Int:\\n    0\\n",
            "pub fn f(n: Net[Connect, Listen]) -> Int:\\n    0\\n",
        );
        assert!(widen.contains("\"widened\":true"), "should widen: {widen}");
        assert!(widen.contains("\"added\":[\"Net[Listen]\"]"), "added wrong: {widen}");
        // The reverse (tightening to connect-only) is a safe narrowing.
        let narrow = diff(
            "pub fn f(n: Net[Connect, Listen]) -> Int:\\n    0\\n",
            "pub fn f(n: Net[Connect]) -> Int:\\n    0\\n",
        );
        assert!(narrow.contains("\"widened\":false"), "should not widen: {narrow}");
        assert!(narrow.contains("\"removed\":[\"Net[Listen]\"]"), "removed wrong: {narrow}");
    }

    /// `std/toml` (pure witchy) reads `witchy.toml` manifests: `toml.get` for
    /// string values by `section.key`, `toml.get_array` for string arrays — what
    /// a self-hosted package manager needs to read a manifest.
    #[test]
    fn toml_module_reads_manifest_values() {
        let src = r#"import toml
import string

fn main(console: Console):
    let m = "[rune]\nname = \"acme/widget\"\nversion = \"1.2.0\"\n\n[capabilities]\nruntime = [\"Net\", \"Console\"]\n"
    print(console, opt(toml.get(m, "rune.name")))
    print(console, opt(toml.get(m, "rune.version")))
    print(console, string.join(toml.get_array(m, "capabilities.runtime"), "|"))
    print(console, opt(toml.get(m, "rune.absent")))

fn opt(o: Option(String)) -> String:
    match o:
        Some(s) -> s
        None -> "(none)"
"#;
        assert_eq!(
            link_run(src),
            vec!["acme/widget", "1.2.0", "Net|Console", "(none)"]
        );
    }

    /// Trailing `# comments` on values and arrays are stripped, but a `#` inside a
    /// quoted string and a `]` inside an array element (e.g. "Dir[Read]") are
    /// preserved — real manifests carry comments, so the reader must tolerate them.
    #[test]
    fn toml_module_ignores_trailing_comments() {
        let src = r#"import toml
import string

fn main(console: Console):
    let m = "[rune]\nname = \"acme/widget\"  # the canonical name\ntag = \"v#1\"  # has a hash inside\n\n[capabilities]\nruntime = [\"Console\", \"Dir[Read]\"]  # what it needs\n"
    print(console, opt(toml.get(m, "rune.name")))
    print(console, opt(toml.get(m, "rune.tag")))
    print(console, string.join(toml.get_array(m, "capabilities.runtime"), "|"))

fn opt(o: Option(String)) -> String:
    match o:
        Some(s) -> s
        None -> "(none)"
"#;
        assert_eq!(
            link_run(src),
            vec!["acme/widget", "v#1", "Console|Dir[Read]"]
        );
    }

    /// The `Clock` capability yields wall-clock time (ms since epoch) via `now`.
    /// Reading the clock is ambient nondeterminism, so it's capability-gated and
    /// surfaces in the footprint — not a pure builtin.
    #[test]
    fn clock_capability_yields_wall_clock_time() {
        let out = interp(
            "fn main(console: Console, clock: Clock):\n    print(console, int_to_string(now(clock)))\n",
        );
        let ms: i64 = out[0].parse().expect("now should print an integer");
        assert!(ms > 1_600_000_000_000, "now should be ms since the Unix epoch (got {ms})");
        // `now` needs a Clock — calling it with another capability is a type error.
        assert!(typeck::check_str("fn main(c: Console):\n    let t = now(c)\n").is_err());
        // The Clock requirement surfaces in the capability footprint.
        let fp = crate::capabilities::analyze(
            &parser::parse_module("fn main(console: Console, clock: Clock):\n    let t = now(clock)\n")
                .expect("parse"),
        );
        assert!(fp.total.contains_key("Clock"), "Clock should appear in the footprint");
    }

    /// The `Env` capability reads process environment variables via `get_env`,
    /// returning `Option(String)` (None when unset). Reading the environment is
    /// ambient authority, so it's capability-gated and surfaces in the footprint.
    #[test]
    fn env_capability_reads_environment_variables() {
        // A definitely-unset variable yields None.
        let out = interp(
            "fn main(console: Console, env: Env):\n    match get_env(env, \"WITCHY_NOPE_UNSET_VAR\"):\n        Some(v) -> print(console, v)\n        None -> print(console, \"unset\")\n",
        );
        assert_eq!(out, vec!["unset"]);
        // `get_env` needs an Env capability — another capability is a type error.
        assert!(typeck::check_str("fn main(c: Console):\n    let x = get_env(c, \"X\")\n").is_err());
        // The Env requirement surfaces in the capability footprint.
        let fp = crate::capabilities::analyze(
            &parser::parse_module("fn main(console: Console, env: Env):\n    let x = get_env(env, \"X\")\n")
                .expect("parse"),
        );
        assert!(fp.total.contains_key("Env"), "Env should appear in the footprint");
    }

    /// `main` may declare a `List(String)` parameter to receive command-line
    /// arguments — argv is input data, not authority, so it's an ordinary value
    /// parameter passed by the host (here `run_module_args`), not a capability.
    #[test]
    fn main_receives_command_line_args() {
        let run = |args: Vec<String>| -> Vec<String> {
            let src = "import string\nfn main(console: Console, args: List(String)):\n    print(console, string.join(args, \",\"))\n";
            let module = parser::parse_module(src).expect("parse");
            let linked = crate::linker::link(vec![("main".into(), module)], "main").expect("link");
            typeck::check(&linked).expect("typecheck");
            interpreter::run_module_args(linked, ".", Vec::new(), args).expect("run")
        };
        assert_eq!(run(vec!["a".into(), "b".into(), "c".into()]), vec!["a,b,c"]);
        assert_eq!(run(Vec::new()), vec![""]); // empty argv -> empty list -> ""
    }

    /// The reference interpreter and the compiled WASM backend must produce the
    /// same output for the same program — the core promise of witchy's two-tier
    /// design. This differential test exercises a spread of features and asserts
    /// agreement directly (no hardcoded expectations), so a future codegen change
    /// that silently diverges from the interpreter is caught. Programs stay
    /// within the compiled backend's supported semantics (notably 32-bit Int).
    #[test]
    fn interpreter_and_compiled_backends_agree() {
        let programs: &[(&str, &str)] = &[
            (
                "arithmetic + control flow",
                r#"
fn main(console: Console):
    var acc = 0
    var i = 0
    while (i < 12):
        if ((i % 2) == 0):
            acc = (acc + i)
        else:
            acc = (acc - i)
        i = (i + 1)
    print(console, int_to_string(acc))
"#,
            ),
            (
                "records + update + field access",
                r#"
type Point:
    x: Int
    y: Int

fn main(console: Console):
    let p = Point(3, 4)
    let q = Point(x: ((p).x + 10), ..p)
    print(console, int_to_string(((q).x * (q).y)))
"#,
            ),
            (
                "lists + recursion + head/tail match",
                r#"
fn sum(xs: List(Int)) -> Int:
    match xs:
        [] -> 0
        [h, ..t] -> (h + sum(t))

fn main(console: Console):
    print(console, int_to_string(sum([1, 2, 3, 4, 5])))
"#,
            ),
            (
                "ADTs + match",
                r#"
type Shape:
    Circle(Int)
    Rect(Int, Int)

fn area(s: Shape) -> Int:
    match s:
        Circle(r) -> ((3 * r) * r)
        Rect(w, h) -> (w * h)

fn main(console: Console):
    print(console, int_to_string((area(Circle(5)) + area(Rect(3, 4)))))
"#,
            ),
            (
                "capturing closures + higher-order",
                r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main(console: Console):
    let k = 100
    print(console, int_to_string(apply(fn(n: Int): (n + k), 5)))
"#,
            ),
            (
                "dicts",
                r#"
fn main(console: Console):
    var d = dict_new()
    d = insert(d, "a", 1)
    d = insert(d, "b", 2)
    d = insert(d, "a", 9)
    print(console, int_to_string((get_or(d, "a", 0) + size(d))))
"#,
            ),
            (
                "strings",
                r#"
fn main(console: Console):
    print(console, replace("a,b,c", ",", "-"))
    print(console, int_to_string(index_of("hello", "l")))
    print(console, substring("hello", 1, 4))
    for w in split("the cat sat", " "):
        print(console, w)
"#,
            ),
            (
                "string equality across a List(String) parameter",
                r#"
fn count_matches(words: List(String), target: String) -> Int:
    var n = 0
    for w in words:
        if (w == target):
            n = (n + 1)
    n

fn main(console: Console):
    let words = split("apple banana apple cherry apple", " ")
    print(console, int_to_string(count_matches(words, "apple")))
"#,
            ),
            (
                "string equality + ordering",
                r#"
fn main(console: Console):
    let a = substring("xapple", 1, 6)
    print(console, to_string((a == "apple")))
    print(console, to_string((a == "apricot")))
    print(console, to_string((a != "apricot")))
    print(console, to_string(("apple" < "banana")))
    print(console, to_string(("banana" < "apple")))
    print(console, to_string(("app" < "apple")))
    print(console, to_string(("apple" <= "apple")))
"#,
            ),
            (
                "tuples + polymorphic to_string",
                r#"
fn main(console: Console):
    let (a, b) = (7, 8)
    print(console, to_string((a + b)))
    print(console, to_string((a < b)))
    print(console, to_string("done"))
"#,
            ),
        ];
        for (name, src) in programs {
            let interpreted = interp(src);
            let compiled = run_on_wasm(src);
            assert_eq!(
                interpreted, compiled,
                "interpreter and compiled backends diverged for `{name}`"
            );
        }
    }

    /// The std `list` library is the most-exercised witchy code; verify a broad
    /// slice of it (reverse/take/drop/sort_by/zip/enumerate/map/filter/fold/
    /// index_of/contains/any/all) produces identical results in the interpreter
    /// and compiled to WASM. Int element lists keep this clear of the known
    /// generic-`==`-on-strings limitation (compiled compares those by pointer).
    #[test]
    fn std_list_library_backends_agree() {
        let client = r#"
import list

fn main(console: Console):
    let xs = [5, 3, 8, 1, 9, 2]
    let rev = list.reverse(xs)
    print(console, ((int_to_string(at(rev, 0)) <> ",") <> int_to_string(at(rev, 5))))
    print(console, ((int_to_string(length(list.take(xs, 3))) <> ":") <> int_to_string(at(list.take(xs, 3), 2))))
    print(console, int_to_string(at(list.drop(xs, 4), 0)))
    let sorted = list.sort_by(xs, fn(a: Int, b: Int): (a < b))
    print(console, ((int_to_string(at(sorted, 0)) <> "..") <> int_to_string(at(sorted, 5))))
    let pairs = list.zip([1, 2, 3], [10, 20, 30])
    let (pa, pb) = at(pairs, 1)
    print(console, int_to_string((pa + pb)))
    let en = list.enumerate([100, 200])
    let (ei, ev) = at(en, 1)
    print(console, int_to_string(((ei * 1000) + ev)))
    let doubled = list.map(xs, fn(n: Int): (n * 2))
    let evens = list.filter(xs, fn(n: Int): ((n % 2) == 0))
    print(console, int_to_string(list.fold(doubled, 0, fn(a: Int, b: Int): (a + b))))
    print(console, int_to_string(length(evens)))
    print(console, int_to_string(list.index_of(xs, 8)))
    print(console, to_string(list.contains(xs, 9)))
    print(console, to_string(list.any(xs, fn(n: Int): (n > 8))))
    print(console, to_string(list.all(xs, fn(n: Int): (n > 0))))
    print(console, int_to_string(list.sum(xs)))
    print(console, to_string(list.is_empty(xs)))
    print(console, to_string(list.is_empty(list.filter(xs, fn(n: Int): (n > 100)))))
    print(console, int_to_string(list.count(xs, fn(n: Int): ((n % 2) == 0))))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(
            interpreted, compiled,
            "std list library diverged between interpreter and compiled"
        );
    }

    #[test]
    fn generic_function_with_match_body_runs_at_multiple_types() {
        // A *single* generic function whose body binds its type param (a match)
        // may be called at different type instantiations in one program. `unwrap`
        // is used at Box(Int) and Box(String); both backends agree.
        let src = r#"
type Box:
    Wrap(a)

fn unwrap(b: Box(a), default: a) -> a:
    match b:
        Wrap(v) -> v

fn main(console: Console):
    print(console, int_to_string(unwrap(Wrap(42), 0)))
    print(console, unwrap(Wrap("hello"), "none"))
"#;
        assert_eq!(interp(src), vec!["42", "hello"]);
        assert_eq!(run_on_wasm(src), vec!["42", "hello"]);
    }

    #[test]
    fn try_operator_result_backends_agree() {
        // `?` propagation on Result: the success path unwraps and continues, the
        // failure path short-circuits with the Err. Both backends must agree.
        let client = r#"
import result

fn parse_pos(n: Int) -> Result(Int, String):
    if (n > 0):
        Ok(n)
    else:
        Err("bad")

fn add_two(a: Int, b: Int) -> Result(Int, String):
    let x = (parse_pos(a))?
    let y = (parse_pos(b))?
    Ok((x + y))

fn main(console: Console):
    print(console, int_to_string(result.unwrap_or(add_two(3, 4), 0)))
    print(console, int_to_string(result.unwrap_or(add_two(3, (0 - 1)), 0)))
    print(console, to_string(result.is_err(add_two((0 - 5), 2))))
    print(console, to_string(result.is_ok(add_two(10, 20))))
"#;
        let sources = [("result", crate::bundled_module("result").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "`?` on Result diverged between backends");
    }

    #[test]
    fn try_operator_option_backends_agree() {
        // `?` propagation on Option: short-circuit on None, unwrap on Some.
        let client = r#"
import option

fn first_even(a: Int, b: Int) -> Option(Int):
    let x = (pick_even(a))?
    let y = (pick_even(b))?
    Some((x + y))

fn pick_even(n: Int) -> Option(Int):
    if ((n % 2) == 0):
        Some(n)
    else:
        None

fn main(console: Console):
    print(console, int_to_string(option.unwrap_or(first_even(4, 6), 0)))
    print(console, int_to_string(option.unwrap_or(first_even(4, 7), 0)))
    print(console, to_string(option.is_none(first_even(3, 8))))
"#;
        let sources = [("option", crate::bundled_module("option").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "`?` on Option diverged between backends");
    }

    #[test]
    fn sort_strings_backends_agree() {
        // Sorting strings lexicographically with `sort_by` and a String
        // comparator — exercising string `<` through call_indirect inside
        // insert_sorted — agrees across backends.
        let client = r#"
import list

fn main(console: Console):
    let words = ["cherry", "apple", "banana", "date", "apple"]
    let sorted = list.sort_by(words, fn(a: String, b: String): (a < b))
    for w in sorted:
        print(console, w)
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "string sort diverged between backends");
        assert_eq!(
            compiled,
            vec!["apple", "apple", "banana", "cherry", "date"]
        );
    }

    #[test]
    fn std_list_find_index_backends_agree() {
        // find_index returns the position of the first predicate match, or -1.
        let client = r#"
import list

fn main(console: Console):
    let xs = [3, 8, 1, 9, 4]
    print(console, int_to_string(list.find_index(xs, fn(n: Int): (n > 5))))
    print(console, int_to_string(list.find_index(xs, fn(n: Int): (n > 100))))
    print(console, int_to_string(list.find_index(xs, fn(n: Int): (n == 1))))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "find_index diverged");
        assert_eq!(compiled, vec!["1", "-1", "2"]);
    }

    #[test]
    fn std_list_zip_with_intersperse_backends_agree() {
        // zip_with combines element-wise (stopping at the shorter list);
        // intersperse inserts a separator between elements. Both backends agree.
        let client = r#"
import list

fn main(console: Console):
    let sums = list.zip_with([1, 2, 3], [10, 20], fn(a: Int, b: Int): (a + b))
    print(console, int_to_string(length(sums)))
    print(console, int_to_string(list.sum(sums)))
    let spaced = list.intersperse([5, 6, 7], 0)
    print(console, int_to_string(length(spaced)))
    print(console, int_to_string(list.sum(spaced)))
    print(console, int_to_string(length(list.intersperse([9], 0))))
    print(console, int_to_string(length(list.intersperse([], 0))))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "zip_with/intersperse diverged");
        assert_eq!(compiled, vec!["2", "33", "5", "18", "1", "0"]);
    }

    #[test]
    fn std_list_take_drop_while_repeat_backends_agree() {
        // take_while/drop_while split at the first failing element; repeat makes
        // n copies. Both backends agree.
        let client = r#"
import list

fn main(console: Console):
    let xs = [1, 2, 3, 10, 4, 5]
    print(console, int_to_string(list.sum(list.take_while(xs, fn(n: Int): (n < 5)))))
    print(console, int_to_string(list.sum(list.drop_while(xs, fn(n: Int): (n < 5)))))
    let threes = list.repeat(7, 3)
    print(console, int_to_string(list.sum(threes)))
    print(console, int_to_string(length(threes)))
    print(console, int_to_string(length(list.repeat(9, 0))))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "take_while/drop_while/repeat diverged");
        assert_eq!(compiled, vec!["6", "19", "21", "3", "0"]);
    }

    #[test]
    fn std_list_flatten_flatmap_backends_agree() {
        // flatten / flat_map (concat-based, with a list-returning closure for
        // flat_map) behave identically in both backends.
        let client = r#"
import list

fn main(console: Console):
    let nested = [[1, 2], [3], [4, 5, 6]]
    let flat = list.flatten(nested)
    print(console, int_to_string(length(flat)))
    print(console, int_to_string(list.sum(flat)))
    let fm = list.flat_map([1, 2, 3], fn(n: Int): [n, (n * 10)])
    print(console, int_to_string(length(fm)))
    print(console, int_to_string(list.sum(fm)))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "flatten/flat_map diverged");
        assert_eq!(compiled, vec!["6", "21", "6", "66"]);
    }

    #[test]
    fn unwrap_or_else_backends_agree() {
        // Lazy defaults via a zero-arg closure, for both Option and Result.
        let opt = r#"
import option

fn main(console: Console):
    print(console, int_to_string(option.unwrap_or_else(Some(5), fn(): 0)))
    let fallback = 99
    print(console, int_to_string(option.unwrap_or_else(option.filter(Some(3), fn(n: Int): (n > 10)), fn(): fallback)))
"#;
        let osrc = [("option", crate::bundled_module("option").unwrap()), ("main", opt)];
        assert_eq!(
            interpreter::run_program(&osrc, "main").expect("interp"),
            run_linked_on_wasm(&osrc, "main")
        );
        assert_eq!(run_linked_on_wasm(&osrc, "main"), vec!["5", "99"]);

        let res = r#"
import result

fn checked(n: Int) -> Result(Int, String):
    if (n > 0):
        Ok(n)
    else:
        Err("bad")

fn main(console: Console):
    print(console, int_to_string(result.unwrap_or_else(checked(7), fn(): 0)))
    print(console, int_to_string(result.unwrap_or_else(checked((0 - 1)), fn(): 42)))
"#;
        let rsrc = [("result", crate::bundled_module("result").unwrap()), ("main", res)];
        assert_eq!(
            interpreter::run_program(&rsrc, "main").expect("interp"),
            run_linked_on_wasm(&rsrc, "main")
        );
        assert_eq!(run_linked_on_wasm(&rsrc, "main"), vec!["7", "42"]);
    }

    #[test]
    fn std_option_combinators_backends_agree() {
        // is_none / and_then / filter behave identically in both backends.
        let client = r#"
import option

fn main(console: Console):
    let s = Some(5)
    print(console, to_string(option.is_none(s)))
    print(console, to_string(option.is_none(option.filter(s, fn(n: Int): (n > 10)))))
    let chained = option.and_then(s, fn(n: Int): Some((n * 2)))
    print(console, int_to_string(option.unwrap_or(chained, 0)))
    let kept = option.filter(s, fn(n: Int): (n > 0))
    print(console, int_to_string(option.unwrap_or(kept, 0)))
"#;
        let sources = [("option", crate::bundled_module("option").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "option combinators diverged");
    }

    // flatten collapses Option(Option(a)) one level; zip pairs two options into
    // Option((a, b)) only when both are Some. Both backends agree.
    #[test]
    fn std_option_flatten_zip_backends_agree() {
        let client = r#"
import option

fn nested(n: Int) -> Option(Option(Int)):
    if (n > 0):
        Some(Some(n))
    else:
        Some(None)

fn main(console: Console):
    print(console, int_to_string(option.unwrap_or(option.flatten(nested(7)), (0 - 1))))
    print(console, int_to_string(option.unwrap_or(option.flatten(nested(0)), (0 - 1))))
    match option.zip(Some(3), Some(4)):
        Some(pair) ->
            let (x, y) = pair
            print(console, int_to_string((x + y)))
        None -> print(console, "none")
    print(console, to_string(option.is_none(option.zip(Some(1), None))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "option flatten/zip diverged");
        assert_eq!(compiled, vec!["7", "-1", "7", "true"]);
    }

    #[test]
    fn std_option_or_mapor_backends_agree() {
        // The fallback combinators: `or` / `or_else` keep a Some or supply an
        // alternative (eagerly / lazily), and `map_or` transforms a Some or
        // returns the default for None. Both backends agree.
        let client = r#"
import option

fn main(console: Console):
    print(console, int_to_string(option.unwrap_or(option.or(Some(5), Some(9)), 0)))
    print(console, int_to_string(option.unwrap_or(option.or(None, Some(9)), 0)))
    print(console, int_to_string(option.unwrap_or(option.or_else(None, fn(): Some(7)), 0)))
    print(console, int_to_string(option.unwrap_or(option.or_else(Some(3), fn(): Some(7)), 0)))
    print(console, int_to_string(option.map_or(Some(10), 0, fn(x: Int): (x * 2))))
    print(console, int_to_string(option.map_or(None, 99, fn(x: Int): (x * 2))))
"#;
        let sources = [("option", crate::bundled_module("option").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "option or/map_or diverged");
        assert_eq!(compiled, vec!["5", "9", "7", "3", "20", "99"]);
    }

    #[test]
    fn std_eq_member_backends_agree() {
        // The Eq trait + the bounded `member` / `index_of` give content-correct
        // equality on BOTH backends — even for runtime-BUILT strings, where a
        // generic `==` search does pointer comparison in compiled code and would
        // wrongly miss. A user `impl Eq` (Box) works, as does the default `ne`.
        let client = r#"
import eq

type Box:
    Box(Int)

impl Eq for Box:
    fn eq(self, other: Self) -> Bool:
        match self:
            Box(a) -> match other:
                Box(b) -> (a == b)

fn build(s: String) -> String:
    var acc = ""
    var i = 0
    while (i < char_count(s)):
        acc = (acc <> substring(s, i, (i + 1)))
        i = (i + 1)
    acc

fn main(console: Console):
    let words = [build("apple"), build("banana")]
    print(console, to_string(eq.member(words, build("banana"))))
    print(console, to_string(eq.member(words, build("cherry"))))
    print(console, int_to_string(eq.index_of([10, 20, 30], 20)))
    print(console, int_to_string(eq.index_of([10, 20, 30], 99)))
    print(console, to_string(eq.member([Box(1), Box(2)], Box(2))))
    print(console, to_string(ne(Box(1), Box(2))))
    print(console, to_string(ne(Box(2), Box(2))))
"#;
        let sources = [("eq", crate::bundled_module("eq").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std eq member/index_of diverged");
        assert_eq!(compiled, vec!["true", "false", "1", "-1", "true", "true", "false"]);
    }

    #[test]
    fn std_eq_count_unique_backends_agree() {
        // `eq.count` / `eq.unique` dispatch through the element type's Eq impl, so
        // they are content-correct on BOTH backends — including runtime-built
        // strings, where `list.unique`'s generic `==` compares pointers and fails
        // to dedupe in compiled code. A user `impl Eq` works too (Tag).
        let client = r#"
import eq
import string

type Tag:
    Tag(Int)

impl Eq for Tag:
    fn eq(self, other: Self) -> Bool:
        match self:
            Tag(a) -> match other:
                Tag(b) -> (a == b)

fn build(s: String) -> String:
    var acc = ""
    var i = 0
    while (i < char_count(s)):
        acc = (acc <> substring(s, i, (i + 1)))
        i = (i + 1)
    acc

fn main(console: Console):
    let words = [build("a"), build("b"), build("a"), build("c"), build("b"), build("a")]
    print(console, int_to_string(eq.count(words, build("a"))))
    print(console, int_to_string(eq.count(words, build("z"))))
    print(console, string.join(eq.unique(words), ","))
    print(console, int_to_string(length(eq.unique([Tag(1), Tag(2), Tag(1), Tag(2), Tag(3)]))))
    print(console, int_to_string(eq.count([Tag(1), Tag(2), Tag(1)], Tag(1))))
"#;
        let sources = [
            ("eq", crate::bundled_module("eq").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std eq count/unique diverged");
        assert_eq!(compiled, vec!["3", "0", "a,b,c", "3", "2"]);
    }

    #[test]
    fn std_set_operations_backends_agree() {
        // Set ops dispatch through Eq (cross-module: set -> eq.member, both
        // bounded generics), so they are content-correct on both backends for
        // runtime-built strings and a user Eq type (Id), and dedupe along the way.
        let client = r#"
import set
import string

type Id:
    Id(Int)

impl Eq for Id:
    fn eq(self, other: Self) -> Bool:
        match self:
            Id(a) -> match other:
                Id(b) -> (a == b)

fn build(s: String) -> String:
    var acc = ""
    var i = 0
    while (i < char_count(s)):
        acc = (acc <> substring(s, i, (i + 1)))
        i = (i + 1)
    acc

fn main(console: Console):
    let a = [build("x"), build("y"), build("x")]
    let b = [build("y"), build("z")]
    print(console, string.join(set.union(a, b), ","))
    print(console, string.join(set.intersection(a, b), ","))
    print(console, string.join(set.difference(a, b), ","))
    print(console, to_string(set.is_subset([build("y")], a)))
    print(console, to_string(set.is_subset([build("z")], a)))
    print(console, int_to_string(length(set.union([Id(1), Id(2), Id(1)], [Id(2), Id(3)]))))
"#;
        let sources = [
            ("set", crate::bundled_module("set").unwrap()),
            ("eq", crate::bundled_module("eq").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std set ops diverged");
        assert_eq!(
            compiled,
            vec!["x,y,z", "y", "x", "true", "false", "3"]
        );
    }

    #[test]
    fn std_ascii_classification_backends_agree() {
        // ASCII predicates are implemented purely via string comparison, so they
        // must agree across the interpreter and the compiled backend. Also drives
        // a tiny tokenizer-style use: sum the digit values in a string.
        let client = r#"
import ascii
import string

fn digit_sum(s: String) -> Int:
    var total = 0
    var i = 0
    while (i < char_count(s)):
        let c = string.char_at(s, i)
        if ascii.is_digit(c):
            total = (total + ascii.to_digit(c))
        i = (i + 1)
    total

fn main(console: Console):
    print(console, to_string(ascii.is_digit("7")))
    print(console, to_string(ascii.is_digit("x")))
    print(console, to_string(ascii.is_alpha("Q")))
    print(console, to_string(ascii.is_alnum("_")))
    print(console, to_string(ascii.is_space("\t")))
    print(console, int_to_string(ascii.to_digit("4")))
    print(console, int_to_string(ascii.to_digit("z")))
    print(console, int_to_string(digit_sum("a1b2c3")))
    print(console, to_string(ascii.all_digits("12345")))
    print(console, to_string(ascii.all_digits("12a45")))
    print(console, to_string(ascii.all_digits("")))
    print(console, to_string(ascii.all_digits("0")))
"#;
        let sources = [
            ("ascii", crate::bundled_module("ascii").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std ascii diverged");
        assert_eq!(
            compiled,
            vec![
                "true", "false", "true", "false", "true", "4", "-1", "6", // all_digits:
                "true", "false", "false", "true",
            ]
        );
    }

    #[test]
    fn std_show_list_backends_agree() {
        // `show.show_list` renders a list via the element type's Show impl, so it
        // works for a user type (Coord) that the built-in to_string cannot print.
        // Monomorphized dispatch keeps it content-correct on both backends.
        let client = r#"
import show

type Coord:
    Coord(Int, Int)

impl Show for Coord:
    fn show(self) -> String:
        match self:
            Coord(x, y) -> (((("(" <> int_to_string(x)) <> ",") <> int_to_string(y)) <> ")")

fn main(console: Console):
    print(console, show.show_list([1, 2, 3]))
    print(console, show.show_list(["a", "b"]))
    print(console, show.show_list([Coord(0, 0), Coord(1, 2)]))
    print(console, show.show_list([true, false]))
"#;
        let sources = [
            ("show", crate::bundled_module("show").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std show_list diverged");
        assert_eq!(
            compiled,
            vec!["[1, 2, 3]", "[a, b]", "[(0,0), (1,2)]", "[true, false]"]
        );
    }

    #[test]
    fn multi_statement_match_arm_body_indented() {
        // A match arm with a multi-statement body, brace-free: `Pat ->` opens an
        // indented block. Both backends agree.
        let client = "type Cmd:\n    Inc\n    Dec\n\nfn apply(n: Int, c: Cmd) -> Int:\n    match c:\n        Inc ->\n            let m = n + 1\n            m\n        Dec ->\n            n - 1\n\nfn main(console: Console):\n    print(console, int_to_string(apply(10, Inc)))\n    print(console, int_to_string(apply(10, Dec)))\n";
        assert_eq!(interp(client), vec!["11", "9"]);
        assert_eq!(run_on_wasm(client), vec!["11", "9"]);
    }

    #[test]
    fn brace_free_record_update_form() {
        // `update e: field = value ...` — brace-free record update (one or more
        // whitespace-separated `name = value` overrides). Both backends agree.
        let client = r#"
type Point:
    x: Int
    y: Int

fn main(console: Console):
    let p = Point(1, 2)
    let q = Point(x: ((p).x + 10), ..p)
    print(console, int_to_string(((q).x + (q).y)))
    let r = Point(x: 5, y: 6, ..p)
    print(console, int_to_string(((r).x + (r).y)))
"#;
        assert_eq!(interp(client), vec!["13", "11"]);
        assert_eq!(run_on_wasm(client), vec!["13", "11"]);
    }

    #[test]
    fn inline_if_else_expression_form() {
        // Brace-free inline `if c: a else: b` (chained), here inside a brace-free
        // lambda inside call parens. Both backends agree.
        let client = r#"
import list

fn main(console: Console):
    let xs = [3, (0 - 2), 0, 5]
    let signs = list.map(xs, fn(n: Int): if (n > 0): 1 else: if (n < 0): (0 - 1) else: 0)
    print(console, int_to_string(list.fold(signs, 0, fn(a: Int, b: Int): (a + b))))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "inline if-else diverged");
        assert_eq!(compiled, vec!["1"]);
    }

    #[test]
    fn brace_free_lambda_form() {
        // `fn(params): expr` — the brace-free single-expression lambda, used
        // inline inside call parens where layout is suppressed. Both backends.
        let client = r#"
import list

fn main(console: Console):
    let xs = [1, 2, 3, 4]
    let doubled = list.map(xs, fn(n: Int): (n * 2))
    print(console, int_to_string(list.fold(doubled, 0, fn(a: Int, b: Int): (a + b))))
    print(console, int_to_string(length(list.filter(xs, fn(n: Int): ((n % 2) == 0)))))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "brace-free lambda diverged");
        assert_eq!(compiled, vec!["20", "2"]);
    }

    #[test]
    fn inherent_impl_in_indentation_syntax() {
        // The inherent impl works under the off-side rule too: `impl Point:`.
        let client = "type Point:\n    Point(Int, Int)\n\nimpl Point:\n    fn sum(self) -> Int:\n        match self:\n            Point(x, y) -> x + y\n\nfn main(console: Console):\n    print(console, int_to_string(sum(Point(4, 5))))\n";
        assert_eq!(interp(client), vec!["9"]);
        assert_eq!(run_on_wasm(client), vec!["9"]);
    }

    #[test]
    fn inherent_impl_methods_dispatch_by_type() {
        // `impl Type { fn m(self) ... }` (no trait) defines methods dispatched by
        // receiver type, reusing the trait machinery. Two types share the method
        // name `mag`; each call resolves to the right one. Both backends agree.
        let client = r#"
type Point:
    Point(Int, Int)

type Circle:
    Circle(Int)

impl Point:
    fn mag(self) -> Int:
        match self:
            Point(x, y) -> ((x * x) + (y * y))

impl Circle:
    fn mag(self) -> Int:
        match self:
            Circle(r) -> (r * r)

fn main(console: Console):
    print(console, int_to_string(mag(Point(3, 4))))
    print(console, int_to_string(mag(Circle(6))))
"#;
        assert_eq!(interp(client), vec!["25", "36"]);
        assert_eq!(run_on_wasm(client), vec!["25", "36"]);
    }

    #[test]
    fn recursive_trait_dispatch_on_match_bound_fields() {
        // A trait method can now dispatch on a variable bound by a constructor
        // pattern when the field type is concrete: `show(x)` / `show(c)` inside a
        // Show impl resolve through the match arm. Covers a nested struct (Named
        // holds a Coord) and stays content-correct on both backends.
        let client = r#"
import show

type Coord:
    Coord(Int, Int)

impl Show for Coord:
    fn show(self) -> String:
        match self:
            Coord(x, y) -> (((("(" <> show(x)) <> ", ") <> show(y)) <> ")")

type Named:
    Named(String, Coord)

impl Show for Named:
    fn show(self) -> String:
        match self:
            Named(label, c) -> ((label <> "=") <> show(c))

fn main(console: Console):
    print(console, show(Coord(3, 4)))
    print(console, show(Named("p", Coord(1, 2))))
    print(console, show.show_list([Coord(0, 0), Coord(5, 6)]))
"#;
        let sources = [
            ("show", crate::bundled_module("show").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "recursive show dispatch diverged");
        assert_eq!(compiled, vec!["(3, 4)", "p=(1, 2)", "[(0, 0), (5, 6)]"]);
    }

    #[test]
    fn json_encode_pretty_backends_agree() {
        let client = r#"
import json
fn main(console: Console):
    let doc = JsonObject([("name", JsonString("witchy")), ("tags", JsonArray([JsonInt(1), JsonInt(2)])), ("empty", JsonArray([]))])
    print(console, json.encode_pretty(doc))
"#;
        let sources = [("json", crate::bundled_module("json").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "encode_pretty diverged");
        assert_eq!(
            compiled,
            vec!["{\n  \"name\": \"witchy\",\n  \"tags\": [\n    1,\n    2\n  ],\n  \"empty\": []\n}"]
        );
    }

    #[test]
    fn json_as_object_backends_agree() {
        // as_object exposes an object's key/value pairs for iteration when the
        // keys aren't known ahead of time; a non-object yields None.
        let client = r#"
import json
import option
fn main(console: Console):
    match json.decode("{\"a\": 1, \"b\": 2}"):
        Ok(doc) ->
            match json.as_object(doc):
                Some(pairs) ->
                    for p in pairs:
                        let (k, _v) = p
                        print(console, k)
                None -> print(console, "not object")
        Err(_e) -> print(console, "err")
    print(console, if option.is_none(json.as_object(JsonInt(5))): "none" else: "some")
"#;
        let sources = [
            ("json", crate::bundled_module("json").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "as_object diverged");
        assert_eq!(compiled, vec!["a", "b", "none"]);
    }

    #[test]
    fn list_range_between_and_step_backends_agree() {
        // range_between is the half-open lo..hi; range_step counts by `step`,
        // ascending or descending, and yields [] when step is 0.
        let client = r#"
import list
import string
fn show_ints(xs: List(Int)) -> String:
    string.join(list.map(xs, fn(n: Int): int_to_string(n)), ",")
fn main(console: Console):
    print(console, show_ints(list.range_between(2, 6)))
    print(console, show_ints(list.range_between(5, 5)))
    print(console, show_ints(list.range_step(0, 10, 3)))
    print(console, show_ints(list.range_step(5, 0, -2)))
    print(console, show_ints(list.range_step(0, 5, 0)))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "range_between/range_step diverged");
        assert_eq!(compiled, vec!["2,3,4,5", "", "0,3,6,9", "5,3,1", ""]);
    }

    #[test]
    fn set_symmetric_difference_and_disjoint_backends_agree() {
        // symmetric_difference composes difference+union (so it de-dups);
        // is_disjoint is true exactly when the intersection is empty.
        let client = r#"
import set
import list
import string
fn show_ints(xs: List(Int)) -> String:
    string.join(list.map(xs, fn(n: Int): int_to_string(n)), ",")
fn main(console: Console):
    print(console, show_ints(set.symmetric_difference([1, 2, 3], [2, 3, 4])))
    print(console, show_ints(set.symmetric_difference([1, 1, 2], [2, 2, 3])))
    print(console, if set.is_disjoint([1, 2], [3, 4]): "yes" else: "no")
    print(console, if set.is_disjoint([1, 2], [2, 3]): "yes" else: "no")
"#;
        let sources = [
            ("eq", crate::bundled_module("eq").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("set", crate::bundled_module("set").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "set ops diverged");
        assert_eq!(compiled, vec!["1,4", "1,3", "yes", "no"]);
    }

    #[test]
    fn math_isqrt_and_perfect_square_backends_agree() {
        // isqrt floors the square root (overflow-safe); is_perfect_square is
        // true exactly on 0,1,4,9,... and false for negatives.
        let client = r#"
import math
import list
import string
fn main(console: Console):
    let roots = list.map([0, 1, 2, 3, 4, 8, 9, 15, 16, 100, 99], fn(n: Int): math.isqrt(n))
    print(console, string.join(list.map(roots, fn(n: Int): int_to_string(n)), ","))
    let flags = list.map([0, 1, 2, 4, 9, 10, 16, 17], fn(n: Int): if math.is_perfect_square(n): "T" else: "F")
    print(console, string.join(flags, ""))
    print(console, int_to_string(math.isqrt(-5)))
    print(console, if math.is_perfect_square(-4): "T" else: "F")
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("math", crate::bundled_module("math").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "isqrt/is_perfect_square diverged");
        assert_eq!(compiled, vec!["0,1,1,1,2,2,3,3,4,10,9", "TTFTTFTF", "0", "F"]);
    }

    #[test]
    fn string_parse_int_backends_agree() {
        // parse_int validates an optional sign + digits before calling the raw
        // string_to_int builtin, so bad input is None (not a trap) consistently.
        let client = r#"
import string
import option
fn show(o: Option(Int)) -> String:
    match o:
        Some(n) -> int_to_string(n)
        None -> "none"
fn main(console: Console):
    print(console, show(string.parse_int("42")))
    print(console, show(string.parse_int("-7")))
    print(console, show(string.parse_int("0")))
    print(console, show(string.parse_int("")))
    print(console, show(string.parse_int("-")))
    print(console, show(string.parse_int("12a")))
    print(console, show(string.parse_int("3.5")))
    print(console, show(string.parse_int(" 5")))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "parse_int diverged");
        assert_eq!(
            compiled,
            vec!["42", "-7", "0", "none", "none", "none", "none", "none"]
        );
    }

    #[test]
    fn string_center_backends_agree() {
        // center pads both sides; an odd remainder goes on the right, and a
        // string already at/over width is returned unchanged.
        let client = r#"
import string
fn main(console: Console):
    print(console, "[" <> string.center("hi", 6, " ") <> "]")
    print(console, "[" <> string.center("hi", 7, " ") <> "]")
    print(console, "[" <> string.center("odd", 8, "*") <> "]")
    print(console, "[" <> string.center("toolong", 4, " ") <> "]")
    print(console, "[" <> string.center("x", 1, " ") <> "]")
"#;
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "center diverged");
        assert_eq!(
            compiled,
            vec!["[  hi  ]", "[  hi   ]", "[**odd***]", "[toolong]", "[x]"]
        );
    }

    #[test]
    fn url_format_roundtrip_backends_agree() {
        // format is parse's inverse; the default port is omitted, a non-default
        // port is kept, and an absent path renders as "/".
        let client = r#"
import url
import option
fn render(s: String) -> String:
    match url.parse(s):
        Some(u) -> url.format(u)
        None -> "no parse"
fn main(console: Console):
    print(console, render("https://example.com/path"))
    print(console, render("http://example.com:8080/x"))
    print(console, render("ftp://host:21/file"))
    print(console, render("http://example.com"))
    print(console, render("not a url"))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("url", crate::bundled_module("url").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "url.format diverged");
        assert_eq!(
            compiled,
            vec![
                "https://example.com/path",
                "http://example.com:8080/x",
                "ftp://host:21/file",
                "http://example.com/",
                "no parse",
            ]
        );
    }

    #[test]
    fn url_parse_rejects_bad_port_without_trapping_backends_agree() {
        // A non-numeric or empty `:port` makes parse return None — it used to trap
        // in string_to_int. A valid or defaulted port still parses, both backends.
        let client = r#"
import url
import option
fn p(s: String) -> String:
    match url.parse(s):
        Some(u) -> "ok:" <> int_to_string(url.port(u))
        None -> "none"
fn main(console: Console):
    print(console, p("https://h:8443/x"))
    print(console, p("https://h:abc/x"))
    print(console, p("https://h:/x"))
    print(console, p("https://h:80x/x"))
    print(console, p("https://h/x"))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("url", crate::bundled_module("url").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "url bad-port diverged");
        assert_eq!(compiled, vec!["ok:8443", "none", "none", "none", "ok:443"]);
    }

    #[test]
    fn func_on_backends_agree() {
        // on(op, f) lifts op to act on projections — here sorting (name, age)
        // pairs by age via func.on_key(lt, snd).
        let client = r#"
import func
import list
import string
fn fst(p: (String, Int)) -> String:
    let (a, _b) = p
    a
fn snd(p: (String, Int)) -> Int:
    let (_a, b) = p
    b
fn lt(a: Int, b: Int) -> Bool:
    a < b
fn main(console: Console):
    let people = [("alice", 30), ("bob", 25), ("carol", 35)]
    let sorted = list.sort_by(people, func.on_key(lt, snd))
    print(console, string.join(list.map(sorted, fst), ","))
    let by_age = func.on_key(lt, snd)
    print(console, if by_age(("x", 1), ("y", 2)): "lt" else: "ge")
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("func", crate::bundled_module("func").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "func.on diverged");
        assert_eq!(compiled, vec!["bob,alice,carol", "lt"]);
    }

    #[test]
    fn json_merge_and_has_key_backends_agree() {
        // merge is a shallow override (b wins per-key; a's other keys kept; a
        // non-object b replaces wholesale); has_key checks top-level presence.
        let client = r#"
import json
fn main(console: Console):
    let a = JsonObject([("name", JsonString("a")), ("x", JsonInt(1))])
    let b = JsonObject([("x", JsonInt(2)), ("y", JsonInt(3))])
    print(console, json.encode(json.merge(a, b)))
    print(console, json.encode(json.merge(a, JsonInt(9))))
    print(console, if json.has_key(a, "x"): "T" else: "F")
    print(console, if json.has_key(a, "z"): "T" else: "F")
    print(console, if json.has_key(JsonInt(5), "x"): "T" else: "F")
"#;
        let sources = [("json", crate::bundled_module("json").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "json.merge/has_key diverged");
        assert_eq!(
            compiled,
            vec!["{\"name\":\"a\",\"x\":2,\"y\":3}", "9", "T", "F", "F"]
        );
    }

    #[test]
    fn config_merge_example_runs_on_wasm() {
        // The layered-config example (json.merge shallow override + encode_pretty)
        // prints identically on both backends: base.debug survives, production
        // overrides host/port and adds workers.
        let sources = [
            ("json", crate::bundled_module("json").unwrap()),
            ("main", include_str!("../examples/config_merge.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "config_merge diverged");
        assert_eq!(
            compiled,
            vec![
                "{\n  \"debug\": true,\n  \"host\": \"example.com\",\n  \"port\": 443,\n  \"workers\": 8\n}",
                "has workers",
            ]
        );
    }

    #[test]
    fn json_decode_rejects_trailing_content_backends_agree() {
        // decode must consume the whole input: trailing whitespace is fine, but
        // any trailing non-whitespace is an error (not a silently-ignored tail).
        let client = r#"
import json
fn classify(s: String) -> String:
    match json.decode(s):
        Ok(j) ->
            match json.as_int(j):
                Some(n) -> "int:" <> int_to_string(n)
                None -> "ok"
        Err(_e) -> "err"
fn main(console: Console):
    print(console, classify("[1, 2]"))
    print(console, classify("42  "))
    print(console, classify("1 2"))
    print(console, classify("true xyz"))
    print(console, classify("{}extra"))
    print(console, classify("  7"))
"#;
        let sources = [("json", crate::bundled_module("json").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "decode trailing-content diverged");
        assert_eq!(compiled, vec!["ok", "int:42", "err", "err", "err", "int:7"]);
    }

    #[test]
    fn json_decode_rejects_floats_with_a_clear_message_backends_agree() {
        // The decoder is integer-only; a fractional/exponent number must be
        // rejected with a clear message, not mis-parsed into "expected , or }".
        let client = r#"
import json
fn classify(s: String) -> String:
    match json.decode(s):
        Ok(j) ->
            match json.as_int(j):
                Some(n) -> "int:" <> int_to_string(n)
                None -> "ok"
        Err(e) ->
            if contains(e, "floats are not supported"):
                "float-err"
            else:
                "err:" <> e
fn main(console: Console):
    print(console, classify("10"))
    print(console, classify("-3"))
    print(console, classify("2.5"))
    print(console, classify("-2.5"))
    print(console, classify("1e3"))
    print(console, classify("{\"a\": 2.5}"))
"#;
        let sources = [("json", crate::bundled_module("json").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "float rejection diverged");
        assert_eq!(
            compiled,
            vec!["int:10", "int:-3", "float-err", "float-err", "float-err", "float-err"]
        );
    }

    #[test]
    fn string_rsplit_once_backends_agree() {
        // rsplit_once splits on the LAST separator (vs split_once's first); when
        // the separator is absent the whole string is the right part.
        let client = r#"
import string
fn show2(p: (String, String)) -> String:
    let (a, b) = p
    a <> "|" <> b
fn main(console: Console):
    print(console, show2(string.rsplit_once("a.b.c", ".")))
    print(console, show2(string.split_once("a.b.c", ".")))
    print(console, show2(string.rsplit_once("nodot", ".")))
    print(console, show2(string.rsplit_once("file.tar.gz", ".")))
    print(console, int_to_string(string.last_index_of("a.b.c", ".")))
    print(console, int_to_string(string.last_index_of("nodot", ".")))
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "rsplit_once diverged");
        assert_eq!(
            compiled,
            vec!["a.b|c", "a|b.c", "|nodot", "file.tar|gz", "3", "-1"]
        );
    }

    #[test]
    fn list_transpose_backends_agree() {
        // transpose swaps rows and columns; a ragged input is truncated to the
        // shortest row, and an empty input gives an empty result.
        let client = r#"
import list
import string
fn show_row(r: List(Int)) -> String:
    string.join(list.map(r, fn(n: Int): int_to_string(n)), ",")
fn show_grid(g: List(List(Int))) -> String:
    string.join(list.map(g, show_row), ";")
fn main(console: Console):
    print(console, show_grid(list.transpose([[1, 2, 3], [4, 5, 6]])))
    print(console, show_grid(list.transpose([[1, 2], [3, 4, 5]])))
    print(console, show_grid(list.transpose([])))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "transpose diverged");
        assert_eq!(compiled, vec!["1,4;2,5;3,6", "1,3;2,4", ""]);
    }

    #[test]
    fn duration_literals_backends_agree() {
        // Native duration literals (1s/1ms/1m/1h/1d/1w, and the `hr` alias) are a
        // distinct Duration type carried as milliseconds: they add/subtract,
        // scale by an Int, divide to an Int ratio, and compare — identically on
        // both backends.
        let client = r#"
fn main(console: Console):
    print(console, to_string(30s > 500ms))
    print(console, to_string(30s + 500ms == 30500ms))
    print(console, to_string(1m == 60s))
    print(console, to_string(2hr == 7200s))
    print(console, to_string(1d == 24h))
    print(console, to_string(1w > 6d))
    print(console, to_string(2 * 1h == 7200s))
    print(console, to_string(1h / 1m == 60))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "duration literals diverged");
        assert_eq!(
            compiled,
            vec!["true", "true", "true", "true", "true", "true", "true", "true"]
        );
    }

    #[test]
    fn durations_example_runs_on_wasm() {
        // The durations example (literals + Duration*Int + comparison + the
        // duration module) prints identically on both backends.
        let sources = [
            ("duration", crate::bundled_module("duration").unwrap()),
            ("main", include_str!("../examples/durations.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "durations example diverged");
        assert_eq!(
            compiled,
            vec!["1s", "2s", "4s", "5s", "5s", "1:30:00", "true", "2m30s"]
        );
    }

    #[test]
    fn random_module_backends_agree() {
        // The Park-Miller LCG replays a deterministic sequence (the canonical
        // seed-1 values) identically on both backends; next_below bounds it.
        let client = r#"
import random
import list
import string
fn main(console: Console):
    var r = random.seed(1)
    var out = []
    var i = 0
    while i < 4:
        let (n, r2) = random.next(r)
        out = push(out, n)
        r = r2
        i = i + 1
    print(console, string.join(list.map(out, fn(n: Int): int_to_string(n)), ","))
    let (d, _r3) = random.next_below(random.seed(42), 6)
    print(console, int_to_string(d))
    let (b, _r4) = random.next_bool(random.seed(2))
    print(console, if b: "even" else: "odd")
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("random", crate::bundled_module("random").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "random diverged");
        assert_eq!(
            compiled,
            vec!["16807,282475249,1622650073,984943658", "0", "even"]
        );
    }

    #[test]
    fn dice_example_runs_on_wasm() {
        // The dice example (seeded random.next_below, threaded Rng) prints the
        // same deterministic rolls on both backends.
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("random", crate::bundled_module("random").unwrap()),
            ("main", include_str!("../examples/dice.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "dice example diverged");
        assert_eq!(compiled, vec!["2 2 1 6 2 2 1 5 2 2", "total: 25"]);
    }

    #[test]
    fn result_and_option_all_backends_agree() {
        // `all` sequences a list of Results/Options: Ok/Some of the collected
        // values, or the first failure (Err / None).
        let client = r#"
import result
import option
import list
import string
fn nums(r: Result(List(Int), String)) -> String:
    match r:
        Ok(xs) -> string.join(list.map(xs, fn(n: Int): int_to_string(n)), ",")
        Err(e) -> "err:" <> e
fn onums(o: Option(List(Int))) -> String:
    match o:
        Some(xs) -> string.join(list.map(xs, fn(n: Int): int_to_string(n)), ",")
        None -> "none"
fn main(console: Console):
    print(console, nums(result.all([Ok(1), Ok(2), Ok(3)])))
    print(console, nums(result.all([Ok(1), Err("bad"), Ok(3)])))
    print(console, nums(result.all([])))
    print(console, onums(option.all([Some(1), Some(2)])))
    print(console, onums(option.all([Some(1), None, Some(3)])))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("result", crate::bundled_module("result").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "result/option.all diverged");
        assert_eq!(compiled, vec!["1,2,3", "err:bad", "", "1,2", "none"]);
    }

    #[test]
    fn result_partition_backends_agree() {
        // partition splits a list of Results into the Ok values and the Err
        // values, each in order.
        let client = r#"
import result
import list
import string
fn main(console: Console):
    let (oks, errs) = result.partition([Ok(1), Err("a"), Ok(2), Err("b"), Ok(3)])
    print(console, string.join(list.map(oks, fn(n: Int): int_to_string(n)), ","))
    print(console, string.join(errs, ","))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("result", crate::bundled_module("result").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "result.partition diverged");
        assert_eq!(compiled, vec!["1,2,3", "a,b"]);
    }

    #[test]
    fn random_choice_backends_agree() {
        // choice picks a uniformly-random element (None for an empty list),
        // deterministically for a given seed, identically on both backends.
        let client = r#"
import random
import option
fn main(console: Console):
    let (c, _r) = random.choice(["a", "b", "c", "d"], random.seed(1))
    print(console, option.unwrap_or(c, "?"))
    let (e, _r2) = random.choice([], random.seed(1))
    print(console, option.unwrap_or(e, "empty"))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("random", crate::bundled_module("random").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "random.choice diverged");
        assert_eq!(compiled, vec!["d", "empty"]);
    }

    #[test]
    fn duration_combinators_backends_agree() {
        // max/min/is_zero/abs over the Duration type (it has no Ord impl, so the
        // generic ord helpers don't apply).
        let client = r#"
import duration
fn main(console: Console):
    print(console, duration.human(duration.max(30s, 1m)))
    print(console, duration.human(duration.min(30s, 1m)))
    print(console, to_string(duration.is_zero(0ms)))
    print(console, to_string(duration.is_zero(1s)))
    print(console, duration.human(duration.abs(0s - 5s)))
"#;
        let sources = [
            ("duration", crate::bundled_module("duration").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "duration combinators diverged");
        assert_eq!(compiled, vec!["1m0s", "30s", "true", "false", "5s"]);
    }

    #[test]
    fn duration_module_backends_agree() {
        // The duration module over the built-in Duration type: human/clock format
        // a Duration (combined from literals), to_millis bridges back to Int.
        let client = r#"
import duration
fn main(console: Console):
    print(console, int_to_string(duration.to_millis(duration.from_hms(1, 2, 3))))
    print(console, duration.clock(1h + 2m + 3s))
    print(console, duration.clock(90s))
    print(console, duration.human(1h + 1m + 1s))
    print(console, duration.human(90s))
    print(console, duration.human(5s))
    print(console, duration.human(500ms))
    print(console, int_to_string(duration.to_millis(duration.hours(2))))
    print(console, int_to_string(duration.part_minutes(1h + 2m + 3s)))
"#;
        let sources = [
            ("duration", crate::bundled_module("duration").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "duration diverged");
        assert_eq!(
            compiled,
            vec![
                "3723000", "1:02:03", "0:01:30", "1h1m1s", "1m30s", "5s", "500ms", "7200000", "2",
            ]
        );
    }

    #[test]
    fn duration_parse_backends_agree() {
        // parse is the inverse of human, returning a Duration (ms): unit-tagged
        // (incl. ms/hr) or bare-ms input, None on junk/dangling, and
        // parse(human(d)) round-trips.
        let client = r#"
import duration
import option
fn show(o: Option(Duration)) -> String:
    match o:
        Some(d) -> int_to_string(duration.to_millis(d))
        None -> "none"
fn roundtrip(d: Duration) -> String:
    match duration.parse(duration.human(d)):
        Some(p) -> if p == d: "ok" else: "bad"
        None -> "none"
fn main(console: Console):
    print(console, show(duration.parse("1h2m3s")))
    print(console, show(duration.parse("500ms")))
    print(console, show(duration.parse("2hr")))
    print(console, show(duration.parse("90")))
    print(console, show(duration.parse("1h30")))
    print(console, show(duration.parse("")))
    print(console, show(duration.parse("abc")))
    print(console, roundtrip(1h + 1m + 1s))
    print(console, roundtrip(90s))
    print(console, roundtrip(250ms))
"#;
        let sources = [
            ("duration", crate::bundled_module("duration").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "duration.parse diverged");
        assert_eq!(
            compiled,
            vec![
                "3723000", "500", "7200000", "90", "none", "none", "none", "ok", "ok", "ok",
            ]
        );
    }

    #[test]
    fn math_ceil_and_round_div_backends_agree() {
        // ceil_div rounds toward +inf for the quotient; round_div rounds to the
        // nearest integer (ties away from zero). Both for a positive divisor.
        let client = r#"
import math
import list
import string
fn show(xs: List(Int)) -> String:
    string.join(list.map(xs, fn(n: Int): int_to_string(n)), ",")
fn main(console: Console):
    print(console, show([math.ceil_div(7, 3), math.ceil_div(6, 3), math.ceil_div(1, 3), math.ceil_div(0, 3)]))
    print(console, show([math.ceil_div(0 - 7, 3), math.ceil_div(0 - 6, 3)]))
    print(console, show([math.round_div(7, 2), math.round_div(5, 3), math.round_div(4, 3), math.round_div(0 - 7, 2)]))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("math", crate::bundled_module("math").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "ceil_div/round_div diverged");
        assert_eq!(compiled, vec!["3,2,1,0", "-2,-2", "4,2,1,-4"]);
    }

    #[test]
    fn math_to_base_backends_agree() {
        // to_base renders a number in base 2..16 (recursively, MSB-first);
        // zero is "0", negatives get a "-", an out-of-range base is "".
        let client = r#"
import math
fn main(console: Console):
    print(console, math.to_hex(255))
    print(console, math.to_hex(0))
    print(console, math.to_hex(4096))
    print(console, math.to_binary(5))
    print(console, math.to_base(255, 16))
    print(console, math.to_base(0 - 255, 16))
    print(console, math.to_base(100, 1))
    print(console, math.to_base(0, 2))
"#;
        let sources = [("math", crate::bundled_module("math").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "to_base diverged");
        assert_eq!(
            compiled,
            vec!["ff", "0", "1000", "101", "ff", "-ff", "", "0"]
        );
    }

    #[test]
    fn math_format_float_backends_agree() {
        // format_float renders a Float at a fixed number of places (rounded
        // half-up) using only float arithmetic, so it works on the compiled
        // backend where the `to_string` builtin cannot format floats.
        let client = r#"
import math
fn main(console: Console):
    print(console, math.format_float(3.14159, 2))
    print(console, math.format_float(0.0 - 0.5, 1))
    print(console, math.format_float(2.0, 0))
    print(console, math.format_float(0.0, 2))
    print(console, math.format_float(1.999, 2))
    print(console, math.format_float(0.0 - 0.04, 1))
    print(console, math.format_float(98.6, 1))
"#;
        let sources = [("math", crate::bundled_module("math").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "format_float diverged");
        assert_eq!(compiled, vec!["3.14", "-0.5", "2", "0.00", "2.00", "0.0", "98.6"]);
    }

    /// The Fahrenheit-to-Celsius table (K&R / Go tour), reproduced in witchy. It
    /// needs real float output — `math.format_float` makes it compile and agree
    /// on both backends, which the float-formatting-less `to_string` could not.
    #[test]
    fn temperature_example_agrees_on_both_backends() {
        let client = std::fs::read_to_string("examples/temperature.witchy").unwrap();
        let sources = [("math", crate::bundled_module("math").unwrap()), ("main", client.as_str())];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "temperature diverged");
        assert_eq!(compiled[0], "0F = -17.8C");
        assert_eq!(compiled[1], "60F = 15.6C");
    }

    #[test]
    fn big_int_arithmetic_backends_agree() {
        // Compiled Int is now i64, so arithmetic beyond the old 32-bit range
        // agrees with the interpreter instead of wrapping.
        let client = r#"
fn main(console: Console):
    let a = 3000000000
    let b = 4000000000
    print(console, int_to_string(a + b))
    print(console, int_to_string(a * 3))
    let big = 9000000000000
    print(console, int_to_string(big))
    print(console, int_to_string(big / 1000))
    print(console, int_to_string(0 - big))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "big-int arithmetic diverged");
        assert_eq!(
            compiled,
            vec![
                "7000000000",
                "9000000000",
                "9000000000000",
                "9000000000",
                "-9000000000000",
            ]
        );
    }

    #[test]
    fn big_ints_in_list_backends_agree() {
        // 8-byte heap slots carry a full i64 Int inside a (concretely-typed) list.
        let client = r#"
fn main(console: Console):
    let xs = [3000000000, 5000000000]
    print(console, int_to_string(at(xs, 0)))
    print(console, int_to_string(at(xs, 1)))
    print(console, int_to_string(at(xs, 0) + at(xs, 1)))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "big-ints-in-list diverged");
        assert_eq!(compiled, vec!["3000000000", "5000000000", "8000000000"]);
    }

    #[test]
    fn floats_in_collections_backends_agree() {
        // 8-byte slots also hold f64, so floats now live in lists and tuples
        // (read back with float_to_int, since Float to_string is still WASM-gated).
        let client = r#"
fn main(console: Console):
    let fs = [1.5, 2.5, 3.5]
    print(console, int_to_string(length(fs)))
    print(console, int_to_string(float_to_int(at(fs, 1))))
    let pair = (1.5, 9.5)
    let (lo, hi) = pair
    print(console, int_to_string(float_to_int(hi)))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "floats-in-collections diverged");
        assert_eq!(compiled, vec!["3", "2", "9"]);
    }

    #[test]
    fn plugin_host_example_runs_on_wasm() {
        // The capability-thesis demo: a list of function-value plugins applied as
        // a pipeline, plus a console-capturing logger closure — identical on both
        // backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("main", include_str!("../examples/plugin_host.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "plugin_host diverged");
        assert_eq!(
            compiled,
            vec!["1 -> 12", "5 -> 20", "10 -> 30", "[log] ran the pipeline"]
        );
    }

    #[test]
    fn bst_example_runs_on_wasm() {
        // The binary search tree (recursive ADT + pattern matching + tree sort)
        // produces identical output on both backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("main", include_str!("../examples/bst.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "bst diverged");
        assert_eq!(
            compiled,
            vec!["1 2 3 4 5 6 7 8 9", "contains 7: true", "contains 10: false"]
        );
    }

    #[test]
    fn generic_stack_example_runs_on_wasm() {
        // A recursive generic ADT `Stack(a)` used at two instantiations (Int and
        // String) with a generic `Option(a)` peek produces identical output on
        // both backends — parametric polymorphism end to end.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("main", include_str!("../examples/generic_stack.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "generic_stack diverged");
        assert_eq!(
            compiled,
            vec![
                "nums size:  3",
                "words size: 2",
                "nums top:   1",
                "words top:  first",
                "rev nums top:  3",
                "rev words top: second",
            ]
        );
    }

    #[test]
    fn let_patterns_example_runs_on_wasm() {
        // `if let` / `while let` desugar to `match`, so the pattern-binding control
        // flow (including draining a list via head/tail in a `while let`) produces
        // identical output on both backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("main", include_str!("../examples/let_patterns.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "let_patterns diverged");
        assert_eq!(
            compiled,
            vec![
                "found: 42",
                "head is 7",
                "pop 1",
                "pop 2",
                "pop 3",
                "pop 4",
                "drained",
            ]
        );
    }

    #[test]
    fn ranges_example_runs_on_wasm() {
        // Integer range patterns (`lo..hi`, `lo..=hi`) desugar to a guarded
        // binding, so the HTTP-status and grade classifiers match identically on
        // both backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("main", include_str!("../examples/ranges.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "ranges diverged");
        assert_eq!(
            compiled,
            vec![
                "200 -> success",
                "204 -> success",
                "301 -> redirect",
                "404 -> client error",
                "503 -> server error",
                "600 -> unknown",
                "95 -> A",
                "83 -> B",
                "71 -> C",
                "42 -> F",
            ]
        );
    }

    #[test]
    fn subscript_example_runs_on_wasm() {
        // `xs[i]` desugars to `at(xs, i)`; chained subscripts index nested lists.
        // The dot product and 2D-grid diagonal match on both backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("main", include_str!("../examples/subscript.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "subscript diverged");
        assert_eq!(
            compiled,
            vec!["dot = 32", "grid[1][2] = 6", "diagonal sum = 15"]
        );
    }

    #[test]
    fn roman_example_runs_on_wasm() {
        // Greedy table walk by subscript (to_roman) and a char scan with the
        // subtractive rule (from_roman) round-trip identically on both backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("main", include_str!("../examples/roman.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "roman diverged");
        assert_eq!(
            compiled,
            vec![
                "4 = IV -> 4",
                "9 = IX -> 9",
                "49 = XLIX -> 49",
                "90 = XC -> 90",
                "1994 = MCMXCIV -> 1994",
                "2024 = MMXXIV -> 2024",
            ]
        );
    }

    #[test]
    fn constants_example_runs_on_wasm() {
        // Top-level constants (including ones built from earlier constants) are
        // inlined before both backends, producing identical output.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("main", include_str!("../examples/constants.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "constants diverged");
        assert_eq!(
            compiled,
            vec![
                "1 hour      = 3600s",
                "1 day       = 86400s",
                "1d 2h 3m 4s = 93784s",
            ]
        );
    }

    #[test]
    fn print_trailing_newline_agrees_on_both_backends() {
        // Regression: a printed string ending in `\n` (the line terminator) must
        // produce identical output on both backends. The WASM host strips a
        // trailing newline; the interpreter now does too.
        let src = "fn main(console: Console):\n    print(console, \"ab\" <> \"\\n\")\n    print(console, \"cd\")\n";
        let sources = [("main", src)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "trailing-newline print diverged");
        assert_eq!(compiled, vec!["ab", "cd"]);
    }

    #[test]
    fn aliases_example_runs_on_wasm() {
        // Type aliases (scalar and compound) are expanded before both backends,
        // so the temperature conversions and averaging agree.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("main", include_str!("../examples/aliases.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "aliases diverged");
        assert_eq!(compiled, vec!["avg C = 21", "25C = 77F", "0C  = 32F"]);
    }

    #[test]
    fn regex_example_runs_on_wasm() {
        // The std/regex backtracking matcher (. * + ? ^ $) produces identical
        // results on both backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("regex", crate::bundled_module("regex").unwrap()),
            ("main", include_str!("../examples/patterns.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "regex diverged");
        assert_eq!(
            compiled,
            vec![
                "match  ^h.*o$  ~  hello",
                "no     ^h.*o$  ~  hi there",
                "match  colou?r  ~  color",
                "match  colou?r  ~  colour",
                "match  ab+a  ~  abbba",
                "no     ab+a  ~  aa",
                "match  cat  ~  the cat sat",
                "no     ^cat  ~  the cat sat",
            ]
        );
    }

    #[test]
    fn calculator_example_runs_on_wasm() {
        // The recursive-descent calculator (mutual recursion + tuple cursors +
        // string scanning) parses and evaluates expressions identically on both
        // backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("main", include_str!("../examples/calculator.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "calculator diverged");
        assert_eq!(
            compiled,
            vec![
                "2 + 3 * 4        = 14",
                "(2 + 3) * 4      = 20",
                "100 - 2 * (3 + 4) = 86",
                "7 + 6 / 2 - 1    = 9",
            ]
        );
    }

    #[test]
    fn pipeline_example_runs_on_wasm() {
        // The method-chained data pipeline (filter/map/sum over list.range)
        // prints identically on both backends.
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", include_str!("../examples/pipeline.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "pipeline diverged");
        assert_eq!(compiled, vec!["120", "0,2,4,6,8"]);
    }

    #[test]
    fn method_call_syntax_backends_agree() {
        // UFCS method chaining: `recv.f(args)` == `f(recv, args)`. The method name
        // resolves to a same-module function (inc) or an imported one (list.*),
        // and reads like a Rust chain. The qualified form still works too.
        let client = r#"
import list
fn inc(x: Int) -> Int:
    x + 1
fn main(console: Console):
    print(console, int_to_string([1, 2, 3, 4].filter(fn(n: Int): n % 2 == 0).map(fn(n: Int): n * 2).sum()))
    print(console, int_to_string(5.inc().inc()))
    print(console, int_to_string(list.sum([10, 20, 30])))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "method-call syntax diverged");
        assert_eq!(compiled, vec!["12", "7", "60"]);
    }

    #[test]
    fn sandbox_runs_compiled_and_captures_output() {
        // `witchy sandbox` compiles to WASM and runs in the capability sandbox,
        // returning the program's output.
        let path = std::env::temp_dir().join("witchy_sandbox_smoke.witchy");
        std::fs::write(
            &path,
            "fn main(console: Console):\n    print(console, int_to_string(6 * 7))\n",
        )
        .unwrap();
        let out = crate::run_file_sandboxed(path.to_str().unwrap()).expect("sandbox run");
        assert_eq!(out, vec!["42"]);
    }

    #[test]
    fn verify_file_agrees_on_a_simple_program() {
        // `witchy verify` runs a program on both backends and confirms identical
        // output; on a normal program that should succeed.
        let path = std::env::temp_dir().join("witchy_verify_smoke.witchy");
        std::fs::write(
            &path,
            "fn main(console: Console):\n    print(console, int_to_string((2 + 3) * 4))\n    print(console, \"hi\")\n",
        )
        .unwrap();
        crate::verify_file(path.to_str().unwrap()).expect("backends should agree");
    }

    #[test]
    fn every_example_type_checks() {
        // Every shipped example must link and type-check (this also exercises
        // import resolution and the constant/alias cycle checks). The parity test
        // skips non-divergence errors, so without this a type error in an example
        // could slip through CI.
        let mut failures = Vec::new();
        for entry in std::fs::read_dir("examples").unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("witchy") {
                continue;
            }
            let p = path.to_str().unwrap();
            if let Err(e) = crate::check_file(p) {
                failures.push(format!("{p}: {e}"));
            }
        }
        assert!(failures.is_empty(), "examples fail to type-check:\n{}", failures.join("\n"));
    }

    #[test]
    fn every_compilable_example_agrees_on_both_backends() {
        // Differential guard: every example that compiles to WASM must produce
        // identical output on the interpreter and the compiled backend. Examples
        // that are interpreter-only (actors, networking, float/case formatting) or
        // are libraries with no `main` cannot compile and are skipped — only a
        // genuine divergence fails. (This would have caught the trailing-newline
        // print divergence.)
        let mut diverged = Vec::new();
        for entry in std::fs::read_dir("examples").unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("witchy") {
                continue;
            }
            let p = path.to_str().unwrap();
            match crate::verify_file(p) {
                Ok(()) => {}
                Err(e) if e.contains("DIVERGE") => diverged.push(e),
                // Interpreter-only feature or no `main`: not comparable, skip.
                Err(_) => {}
            }
        }
        assert!(
            diverged.is_empty(),
            "examples diverge across backends:\n{}",
            diverged.join("\n")
        );
    }

    #[test]
    fn merge_sort_is_stable_on_both_backends() {
        // list.sort_by is a stable merge sort: equal keys keep their original
        // order. Sort (key, tag) items by key only; ties must preserve insertion
        // order. Both backends agree.
        let client = r#"
import list
type Item:
    Item(Int, String)
fn key(it: Item) -> Int:
    match it:
        Item(k, _t) -> k
fn tag(it: Item) -> String:
    match it:
        Item(_k, t) -> t
fn main(console: Console):
    let xs = [Item(2, "a"), Item(1, "b"), Item(2, "c"), Item(1, "d"), Item(2, "e")]
    let sorted = list.sort_by(xs, fn(p: Item, q: Item): key(p) < key(q))
    for it in sorted:
        print(console, int_to_string(key(it)) <> tag(it))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "merge sort diverged");
        assert_eq!(compiled, vec!["1b", "1d", "2a", "2c", "2e"]);
    }

    #[test]
    fn std_ord_string_and_sort_backends_agree() {
        // `impl Ord for String` makes strings comparable, and the bounded generic
        // `ord.sort` dispatches through the element's Ord impl — so it sorts
        // runtime-BUILT strings content-correctly on both backends (a pointer
        // comparison sort would scramble them in compiled code). Also covers
        // Ord-over-String for max_of/maximum and Ints via the same `sort`.
        let client = r#"
import ord
import string

fn build(s: String) -> String:
    var acc = ""
    var i = 0
    while (i < char_count(s)):
        acc = (acc <> substring(s, i, (i + 1)))
        i = (i + 1)
    acc

fn main(console: Console):
    let words = [build("pear"), build("apple"), build("fig"), build("apple")]
    print(console, string.join(ord.sort(words), ","))
    print(console, string.join(ord.sort(["c", "a", "b"]), ""))
    print(console, ord.max_of(build("alpha"), build("omega")))
    print(console, ord.maximum([build("x"), build("a"), build("m")], ""))
    let nums = ord.sort([3, 1, 2, 1])
    print(console, int_to_string((at(nums, 0) + (at(nums, 3) * 10))))
"#;
        let sources = [
            ("ord", crate::bundled_module("ord").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std ord string/sort diverged");
        assert_eq!(
            compiled,
            vec!["apple,apple,fig,pear", "abc", "omega", "x", "31"]
        );
    }

    #[test]
    fn std_result_or_mapor_backends_agree() {
        // The fallback combinators mirror Option's: `or` / `or_else` keep an Ok
        // or supply an alternative (eagerly / error-aware lazily), and `map_or`
        // transforms an Ok or returns the default for an Err. Both backends agree.
        let client = r#"
import result

fn checked(n: Int) -> Result(Int, String):
    if (n > 0):
        Ok(n)
    else:
        Err("bad")

fn main(console: Console):
    print(console, int_to_string(result.unwrap_or(result.or(checked(5), Ok(9)), 0)))
    print(console, int_to_string(result.unwrap_or(result.or(checked((0 - 1)), Ok(9)), 0)))
    print(console, int_to_string(result.unwrap_or(result.or_else(checked((0 - 1)), fn(e: String): Ok(string_length(e))), 0)))
    print(console, int_to_string(result.map_or(checked(5), 0, fn(x: Int): (x * 2))))
    print(console, int_to_string(result.map_or(checked((0 - 1)), 99, fn(x: Int): (x * 2))))
"#;
        let sources = [("result", crate::bundled_module("result").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "result or/map_or diverged");
        assert_eq!(compiled, vec!["5", "9", "3", "10", "99"]);
    }

    #[test]
    fn std_result_combinators_backends_agree() {
        // is_err / and_then / map_err / unwrap_err behave identically in both
        // backends — including using is_err at two different error types in one
        // program (Result(Int, String) and the Result(Int, Int) that map_err
        // produces), which per-call generalization now allows.
        let client = r#"
import result

fn checked(n: Int) -> Result(Int, String):
    if (n > 0):
        Ok(n)
    else:
        Err("bad")

fn main(console: Console):
    print(console, to_string(result.is_err(checked(5))))
    print(console, to_string(result.is_err(checked((0 - 1)))))
    let chained = result.and_then(checked(5), fn(n: Int): Ok((n * 10)))
    print(console, int_to_string(result.unwrap_or(chained, 0)))
    let mapped = result.map_err(checked((0 - 1)), fn(s: String): string_length(s))
    print(console, to_string(result.is_err(mapped)))
"#;
        let sources = [("result", crate::bundled_module("result").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "result combinators diverged");
    }

    fn assert_fn_compiles(src: &str) {
        assert!(typeck::check_str(src).is_ok(), "{:?}", typeck::check_str(src));
        let module = parser::parse_module(src).expect("parse");
        let wat = codegen::compile_module(&module).expect("compile");
        Module::new(&Engine::default(), &wat).expect("valid wasm");
    }

    /// End-to-end through the *compiled* path: type-check, compile to WASM, run
    /// on the wasmtime runtime with the output capabilities granted, and return
    /// what the program printed.
    fn run_on_wasm(src: &str) -> Vec<String> {
        use crate::runtime::{Capabilities, Runtime};
        assert!(typeck::check_str(src).is_ok(), "{:?}", typeck::check_str(src));
        let module = parser::parse_module(src).expect("parse");
        let wat = codegen::compile_module(&module).expect("compile");
        let mut rt = Runtime::new().expect("runtime");
        let mut actor = rt
            .spawn(
                wat.as_bytes(),
                Capabilities {
                    print: true,
                    print_int: true,
                    ..Default::default()
                },
                4,
            )
            .expect("spawn");
        actor.run().expect("run");
        actor.output()
    }

    /// Link a multi-module program, compile the flat module to WASM, run it on
    /// the runtime with output capabilities, and return what it printed.
    fn run_linked_on_wasm(sources: &[(&str, &str)], entry: &str) -> Vec<String> {
        use crate::runtime::{Capabilities, Runtime};
        let mods: Vec<(String, ast::Module)> = sources
            .iter()
            .map(|(n, s)| ((*n).to_string(), parser::parse_module(s).expect("parse")))
            .collect();
        let linked = crate::linker::link(mods, entry).expect("link");
        assert!(typeck::check(&linked).is_ok(), "{:?}", typeck::check(&linked));
        let wat = codegen::compile_module(&linked).expect("compile");
        let mut rt = Runtime::new().expect("runtime");
        let mut actor = rt
            .spawn(
                wat.as_bytes(),
                Capabilities {
                    print: true,
                    print_int: true,
                    ..Default::default()
                },
                4,
            )
            .expect("spawn");
        actor.run().expect("run");
        actor.output()
    }

    #[test]
    fn std_list_compiles_and_runs_on_wasm() {
        // The whole bundled `list` library links + compiles to WASM (every
        // function in it must compile), and map/filter/fold driven by closures
        // run end-to-end: doubled = [2,4,6,8,10] (sum 30); evens = [2,4] (len 2).
        let client = r#"
import list

fn main() -> Int:
    let xs = [1, 2, 3, 4, 5]
    let doubled = list.map(xs, fn(n: Int): (n * 2))
    let evens = list.filter(xs, fn(n: Int): ((n % 2) == 0))
    let sum = list.fold(doubled, 0, fn(acc: Int, n: Int): (acc + n))
    (sum + length(evens))
"#;
        assert_eq!(
            run_linked_on_wasm(
                &[("list", crate::bundled_module("list").unwrap()), ("main", client)],
                "main",
            ),
            vec!["32"]
        );
    }

    #[test]
    fn std_list_sort_by_runs_on_wasm() {
        // A comparator closure threaded through `sort_by` into its `insert_sorted`
        // helper (which calls it via call_indirect) compiles and sorts ascending:
        // [1,1,2,3,4,5,6,9]; first*100 + last = 109.
        let client = r#"
import list

fn main() -> Int:
    let xs = [3, 1, 4, 1, 5, 9, 2, 6]
    let sorted = list.sort_by(xs, fn(a: Int, b: Int): (a < b))
    ((at(sorted, 0) * 100) + at(sorted, 7))
"#;
        assert_eq!(
            run_linked_on_wasm(
                &[("list", crate::bundled_module("list").unwrap()), ("main", client)],
                "main",
            ),
            vec!["109"]
        );
    }

    #[test]
    fn list_pipeline_example_runs_on_wasm() {
        // The example program (import list; map/filter/fold/sort_by + a capturing
        // closure) compiles to WASM and prints identically to the interpreter.
        assert_eq!(
            run_linked_on_wasm(
                &[
                    ("list", crate::bundled_module("list").unwrap()),
                    ("main", include_str!("../examples/list_pipeline.witchy")),
                ],
                "main",
            ),
            vec!["233", "2 8", "735"]
        );
    }

    #[test]
    fn std_math_compiles_and_runs_on_wasm() {
        // Importing `math` forces every function in it to compile (Int helpers
        // *and* the Float ones: fmin/fmax/fabs/fclamp, which use f64 compares and
        // unary negation). gcd(48,36)=12, pow(2,10)=1024, clamp(15,0,10)=10,
        // fclamp(15,0,10)=10.0, fabs(-3.5)=3.5 -> 12+1024+10+10+3 = 1059.
        let client = r#"
import math

fn main() -> Int:
    let a = math.gcd(48, 36)
    let b = math.pow(2, 10)
    let c = math.clamp(15, 0, 10)
    let f = math.fclamp(15.0, 0.0, 10.0)
    let g = math.fabs((0.0 - 3.5))
    ((((a + b) + c) + float_to_int(f)) + float_to_int(g))
"#;
        assert_eq!(
            run_linked_on_wasm(
                &[("math", crate::bundled_module("math").unwrap()), ("main", client)],
                "main",
            ),
            vec!["1059"]
        );
    }

    // factorial (1 for n<=1) and is_prime (trial division; n<2 not prime).
    // A Float-returning main now runs compiled: the auto-print wrapper calls
    // the newly-wired print_float host, which formats f64 exactly like the
    // interpreter's Value::Float Display. Previously the compiled module failed
    // to instantiate (no print_float import provider).
    // A broader compiled-float workout: division, fabs (negation + compare),
    // fmax, a float comparison driving a float-valued `if`, multiply, subtract,
    // and sqrt — all feeding one Float result. Both backends agree.
    #[test]
    fn float_arithmetic_compiled_backends_agree() {
        let client = r#"
import math

fn main() -> Float:
    let a = (10.0 / 4.0)
    let b = math.fabs((0.0 - 1.5))
    let c = math.fmax(a, b)
    let d = if (c > 2.0): (c * 2.0) else: 0.0
    (d - sqrt(4.0))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "compiled float arithmetic diverged");
        assert_eq!(compiled, vec!["3"]);
    }

    #[test]
    fn float_returning_main_backends_agree() {
        let client = r#"
import math

fn main() -> Float:
    (math.fmin(2.5, sqrt(2.25)) + math.fclamp(5.0, 0.0, 1.0))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "float main diverged");
        assert_eq!(compiled, vec!["2.5"]);
    }

    #[test]
    fn std_math_factorial_is_prime_backends_agree() {
        let client = r#"
import math

fn main(console: Console):
    print(console, int_to_string(math.factorial(5)))
    print(console, int_to_string(math.factorial(0)))
    print(console, int_to_string(math.factorial(1)))
    print(console, to_string(math.is_prime(7)))
    print(console, to_string(math.is_prime(12)))
    print(console, to_string(math.is_prime(1)))
    print(console, to_string(math.is_prime(2)))
    print(console, to_string(math.is_prime(97)))
"#;
        let sources = [("math", crate::bundled_module("math").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "factorial/is_prime diverged");
        assert_eq!(compiled, vec!["120", "1", "1", "true", "false", "false", "true", "true"]);
    }

    #[test]
    fn std_math_lcm_parity_backends_agree() {
        // lcm (built on gcd) and the is_even/is_odd predicates agree across
        // backends, including negative operands.
        let client = r#"
import math

fn main(console: Console):
    print(console, int_to_string(math.lcm(4, 6)))
    print(console, int_to_string(math.lcm(21, 6)))
    print(console, int_to_string(math.lcm(0, 5)))
    print(console, int_to_string(math.lcm((0 - 4), 6)))
    print(console, to_string(math.is_even(10)))
    print(console, to_string(math.is_odd(7)))
    print(console, to_string(math.is_odd((0 - 3))))
"#;
        let sources = [("math", crate::bundled_module("math").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "math lcm/parity diverged");
        assert_eq!(compiled, vec!["12", "42", "0", "12", "true", "true", "true"]);
    }

    #[test]
    fn std_result_compiles_and_runs_on_wasm() {
        // The Result type + combinators compile; map_ok runs a closure over Ok.
        let client = r#"
import result

fn main() -> Int:
    let r = result.map_ok(Ok(20), fn(n: Int): (n + 1))
    result.unwrap_or(r, 0)
"#;
        assert_eq!(
            run_linked_on_wasm(
                &[("result", crate::bundled_module("result").unwrap()), ("main", client)],
                "main",
            ),
            vec!["21"]
        );
    }

    #[test]
    fn std_option_compiles_and_runs_on_wasm() {
        // The Option type + combinators compile; map runs a closure over Some.
        let client = r#"
import option

fn main() -> Int:
    let o = option.map(Some(20), fn(n: Int): (n * 2))
    option.unwrap_or(o, 0)
"#;
        assert_eq!(
            run_linked_on_wasm(
                &[("option", crate::bundled_module("option").unwrap()), ("main", client)],
                "main",
            ),
            vec!["40"]
        );
    }

    #[test]
    fn split_runs_on_wasm() {
        // `split` compiled to WASM, matching Rust's str::split: pieces between
        // separators, empty pieces kept, multi-char separators, and an empty
        // separator yielding the whole string.
        let src = r#"
fn main(console: Console):
    let p = split("a,bb,ccc", ",")
    print(console, int_to_string(length(p)))
    print(console, at(p, 0))
    print(console, at(p, 2))
    print(console, int_to_string(length(split("a,,b", ","))))
    print(console, at(split("a,,b", ","), 1))
    print(console, int_to_string(length(split("", ","))))
    print(console, int_to_string(length(split("abc", ""))))
    print(console, at(split("xXXyXXz", "XX"), 2))
"#;
        assert_eq!(
            run_on_wasm(src),
            vec!["3", "a", "ccc", "3", "", "1", "1", "z"]
        );
    }

    #[test]
    fn to_string_polymorphic_on_wasm() {
        // `to_string` renders by the argument's compile-time value type: Int
        // literals/arithmetic, Bool literals/comparisons/user-fn results, and
        // String pass-through — all in compiled code.
        let src = r#"
fn classify(n: Int) -> Bool:
    (n > 0)

fn main(console: Console):
    print(console, to_string(42))
    print(console, to_string((0 - 5)))
    print(console, to_string(true))
    print(console, to_string((3 > 7)))
    print(console, to_string("hi"))
    print(console, to_string(classify(9)))
    let flag = (2 == 2)
    print(console, to_string(flag))
"#;
        assert_eq!(
            run_on_wasm(src),
            vec!["42", "-5", "true", "false", "hi", "true", "true"]
        );
    }

    #[test]
    fn to_string_respects_lambda_param_shadowing_on_wasm() {
        // The outer `x` is an Int; the lambda's `x` is a String param. `to_string`
        // inside the lambda must pass the String through, not run int_to_string on
        // the pointer — i.e. value-type tracking is scoped per lambda.
        let src = r#"
fn apply(f: fn(String) -> String, s: String) -> String:
    f(s)

fn main(console: Console):
    let x = 5
    print(console, to_string(x))
    print(console, apply(fn(x: String): to_string(x), "hey"))
"#;
        assert_eq!(run_on_wasm(src), vec!["5", "hey"]);
    }

    #[test]
    fn to_string_on_undetermined_type_is_rejected() {
        // A value type codegen can't pin down (here a list) errors clearly rather
        // than silently mis-rendering.
        let src = r#"
fn main(console: Console):
    print(console, to_string([1, 2, 3]))
"#;
        let module = parser::parse_module(src).expect("parse");
        let err = codegen::compile_module(&module).expect_err("should reject");
        assert!(
            err.to_string().contains("could not determine"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn negative_int_to_string_on_wasm() {
        // `int_to_string` renders negatives with a leading '-' (previously it
        // emitted garbage, e.g. "/" for -1).
        let src = r#"
fn main(console: Console):
    print(console, int_to_string((0 - 1)))
    print(console, int_to_string((0 - 128)))
    print(console, int_to_string(255))
    print(console, int_to_string(0))
"#;
        assert_eq!(run_on_wasm(src), vec!["-1", "-128", "255", "0"]);
    }

    #[test]
    fn replace_on_wasm() {
        // `replace` compiled to WASM, matching Rust's str::replace: simple and
        // multi-char patterns, greedy non-overlapping, deletion (empty `to`),
        // growth (`to` longer than `from`), no match, an empty `from` (inserted
        // at every char boundary), and UTF-8 (`é` is a 2-byte match).
        let src = r#"
fn main(console: Console):
    print(console, replace("a,b,c", ",", ";"))
    print(console, replace("aXXbXXc", "XX", "-"))
    print(console, replace("aaa", "aa", "x"))
    print(console, replace("a,b,c", ",", ""))
    print(console, replace("abc", "b", "XYZ"))
    print(console, replace("abc", "z", "Q"))
    print(console, replace("ab", "", "-"))
    print(console, replace("café", "é", "e"))
"#;
        assert_eq!(
            run_on_wasm(src),
            vec!["a;b;c", "a-b-c", "xa", "abc", "aXYZc", "abc", "-a-b-", "cafe"]
        );
    }

    #[test]
    fn string_search_slice_on_wasm() {
        // contains / index_of / substring compiled to WASM, matching the
        // interpreter — including Unicode: "café!" has the `!` at character index
        // 4 (byte 5), and substring(3,5) is the two characters "é!".
        let src = r#"
fn main(console: Console):
    print(console, int_to_string(if contains("hello world", "world"): 1 else: 0))
    print(console, int_to_string(if contains("abc", "xyz"): 1 else: 0))
    print(console, int_to_string(if contains("abc", ""): 1 else: 0))
    print(console, int_to_string(index_of("hello", "l")))
    print(console, int_to_string(index_of("hello", "z")))
    print(console, substring("hello", 1, 4))
    print(console, substring("hi", 0, 100))
    print(console, substring("hi", 5, 10))
    print(console, int_to_string(index_of("café!", "!")))
    print(console, substring("café!", 3, 5))
"#;
        assert_eq!(
            run_on_wasm(src),
            vec!["1", "0", "1", "2", "-1", "ell", "hi", "", "4", "é!"]
        );
    }

    #[test]
    fn parse_kv_example_runs_on_wasm() {
        // The `key=value` parser example now compiles end-to-end: index_of +
        // substring + string_length + ends_with + to_string(Bool), matching the
        // interpreter.
        assert_eq!(
            run_on_wasm(include_str!("../examples/parse_kv.witchy")),
            vec!["timeout", "30", "true"]
        );
    }

    #[test]
    fn dict_string_keys_on_wasm() {
        // String-keyed Dict compiled to WASM: insert (append + replace), get_or
        // (present/absent), has, and size — keys compared with $str_eq.
        let src = r#"
fn main(console: Console):
    var d = dict_new()
    d = insert(d, "a", 1)
    d = insert(d, "b", 2)
    d = insert(d, "a", 10)
    print(console, int_to_string(get_or(d, "a", 0)))
    print(console, int_to_string(get_or(d, "b", 0)))
    print(console, int_to_string(get_or(d, "z", (0 - 1))))
    print(console, int_to_string(size(d)))
    print(console, int_to_string(if has(d, "b"): 1 else: 0))
    print(console, int_to_string(if has(d, "q"): 1 else: 0))
"#;
        assert_eq!(run_on_wasm(src), vec!["10", "2", "-1", "2", "1", "0"]);
    }

    #[test]
    fn dict_int_keys_on_wasm() {
        // Int-keyed Dict: keys compared with i32 equality (mode 0).
        let src = r#"
fn main(console: Console):
    var d = dict_new()
    d = insert(d, 1, 100)
    d = insert(d, 2, 200)
    print(console, int_to_string(get_or(d, 1, 0)))
    print(console, int_to_string(get_or(d, 2, 0)))
    print(console, int_to_string(get_or(d, 3, (0 - 1))))
"#;
        assert_eq!(run_on_wasm(src), vec!["100", "200", "-1"]);
    }

    #[test]
    fn wordcount_example_runs_on_wasm() {
        // The word-frequency example compiles to WASM: a String-keyed Dict built
        // in a `for w in split(...)` loop (so `w`'s type resolves to String).
        // the=3, cat=1, missing=0, size=4.
        assert_eq!(
            run_on_wasm(include_str!("../examples/wordcount.witchy")),
            vec!["3", "1", "0", "4"]
        );
    }

    #[test]
    fn dict_undetermined_key_is_rejected() {
        // A key whose type codegen can't pin down (here a list) errors clearly
        // rather than picking a wrong comparison.
        let src = r#"
fn main(console: Console):
    var d = dict_new()
    d = insert(d, [1, 2], 5)
    print(console, int_to_string(size(d)))
"#;
        let module = parser::parse_module(src).expect("parse");
        let err = codegen::compile_module(&module).expect_err("should reject");
        assert!(
            err.to_string().contains("could not determine the Dict key type"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn direct_field_access_on_expressions_backends_agree() {
        // Field access directly on a record-producing expression (no `let`): a
        // constructor literal, a record-returning call, and an `at` result.
        let src = r#"
type Item:
    price: Int
    qty: Int

fn lookup(b: Bool) -> Item:
    if b:
        Item(3, 10)
    else:
        Item(5, 2)

fn main(console: Console):
    print(console, int_to_string((Item(7, 6)).price))
    print(console, int_to_string((lookup(true)).qty))
    let items = [Item(1, 2), Item(3, 4)]
    print(console, int_to_string((at(items, 1)).qty))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["7", "10", "4"]);
    }

    #[test]
    fn conditional_record_field_access_backends_agree() {
        // `let x = if c { A } else { B }; x.field` (and a match-bound record):
        // the binding's record type is recovered from the branch/arm.
        let src = r#"
type Item:
    price: Int
    qty: Int

fn pick(b: Bool) -> Int:
    let x = if b: Item(3, 10) else: Item(5, 2)
    ((x).price * (x).qty)

fn from_tag(t: Int) -> Int:
    let y = match t:
        0 -> Item(1, 1)
        _ -> Item(2, 3)
    ((y).price + (y).qty)

fn main(console: Console):
    print(console, int_to_string(pick(true)))
    print(console, int_to_string(pick(false)))
    print(console, int_to_string(from_tag(0)))
    print(console, int_to_string(from_tag(9)))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["30", "10", "2", "5"]);
    }

    #[test]
    fn list_of_records_index_access_backends_agree() {
        // `at(items, i).field` via a let, for both a List(Record) parameter and a
        // let-bound list literal of records; and a for-loop over the let-bound
        // list. Both backends agree.
        let src = r#"
type Item:
    price: Int
    qty: Int

fn first_value(items: List(Item)) -> Int:
    let first = at(items, 0)
    ((first).price * (first).qty)

fn main(console: Console):
    print(console, int_to_string(first_value([Item(3, 10), Item(5, 2)])))
    let items = [Item(2, 4), Item(7, 1)]
    let second = at(items, 1)
    print(console, int_to_string(((second).price + (second).qty)))
    var total = 0
    for it in items:
        total = (total + (it).price)
    print(console, int_to_string(total))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["30", "8", "9"]);
    }

    #[test]
    fn dict_of_records_field_access_backends_agree() {
        // Looking a record up in a Dict and accessing its field: the result of
        // get_or carries the default's record type, so `it.price` resolves.
        let src = r#"
type Item:
    price: Int
    qty: Int

fn main(console: Console):
    var d = dict_new()
    d = insert(d, "apple", Item(3, 10))
    d = insert(d, "bread", Item(2, 5))
    let it = get_or(d, "apple", Item(0, 0))
    print(console, int_to_string(((it).price * (it).qty)))
    let missing = get_or(d, "milk", Item(0, 0))
    print(console, int_to_string((missing).price))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["30", "0"]);
    }

    #[test]
    fn dict_remove_backends_agree() {
        // `remove` (string and int keys) — present, absent, and the surviving
        // entries — agrees across the interpreter and compiled backends.
        let src = r#"
fn main(console: Console):
    var d = dict_new()
    d = insert(d, "a", 1)
    d = insert(d, "b", 2)
    d = insert(d, "c", 3)
    let d2 = remove(d, "b")
    print(console, int_to_string(size(d2)))
    print(console, int_to_string(if has(d2, "b"): 1 else: 0))
    print(console, int_to_string(get_or(d2, "a", 0)))
    print(console, int_to_string(get_or(d2, "c", 0)))
    let d3 = remove(d, "missing")
    print(console, int_to_string(size(d3)))
    print(console, int_to_string(size(d)))
    var nums = dict_new()
    nums = insert(nums, 10, 100)
    nums = insert(nums, 20, 200)
    let nums2 = remove(nums, 10)
    print(console, int_to_string(size(nums2)))
    print(console, int_to_string(get_or(nums2, 20, 0)))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["2", "0", "1", "3", "3", "3", "1", "200"]);
    }

    #[test]
    fn dict_keys_values_pairs_on_wasm() {
        // keys/values/pairs compiled to WASM: keys -> list of keys, values ->
        // list of values, pairs -> list of (k, v) tuples destructured in a loop.
        let src = r#"
fn main(console: Console):
    var d = dict_new()
    d = insert(d, "a", 10)
    d = insert(d, "b", 20)
    d = insert(d, "c", 30)
    var ksum = 0
    for k in keys(d):
        ksum = (ksum + string_length(k))
    print(console, int_to_string(ksum))
    var vsum = 0
    for v in values(d):
        vsum = (vsum + v)
    print(console, int_to_string(vsum))
    var psum = 0
    for entry in pairs(d):
        let (k, v) = entry
        psum = ((psum + string_length(k)) + v)
    print(console, int_to_string(psum))
"#;
        // keys "a","b","c" (len 1 each) -> 3; values 10+20+30 -> 60; pairs
        // (1+10)+(1+20)+(1+30) -> 63.
        assert_eq!(run_on_wasm(src), vec!["3", "60", "63"]);
    }

    #[test]
    fn inventory_example_runs_on_wasm() {
        // Dict iteration example compiles: `values` summed in a loop, and `pairs`
        // destructured. total = 3+2+4 = 9; prices over 2 = {apple, milk} = 2.
        assert_eq!(
            run_on_wasm(include_str!("../examples/inventory.witchy")),
            vec!["total = 9", "over 2: 2"]
        );
    }

    #[test]
    fn std_string_compiles_and_runs_on_wasm() {
        // With `split` compiled, the whole `string` module compiles: `lines`
        // (split on "\n"), `join`, and `repeat`. lines -> ["a","bb","ccc"] (3);
        // join -> "a-bb-ccc" (8); repeat -> "zzzzz" (5): 3*100 + 8 + 5 = 313.
        let client = r#"
import string

fn main() -> Int:
    let parts = string.lines("a\nbb\nccc")
    let joined = string.join(parts, "-")
    let r = string.repeat("z", 5)
    (((length(parts) * 100) + string_length(joined)) + string_length(r))
"#;
        assert_eq!(
            run_linked_on_wasm(
                &[("string", crate::bundled_module("string").unwrap()), ("main", client)],
                "main",
            ),
            vec!["313"]
        );
    }

    #[test]
    fn std_string_pad_backends_agree() {
        // pad_left/pad_right reach an exact target width, trimming the padding
        // even when `fill` is multi-character; an already-wide string is left
        // untouched. Multi-char fill "-=" padding "ab" to 7 -> "-=-=-ab".
        let client = r#"
import string

fn main(console: Console):
    print(console, string.pad_left("42", 5, "0"))
    print(console, string.pad_right("42", 5, "."))
    print(console, string.pad_left("hello", 3, "x"))
    print(console, string.pad_left("ab", 7, "-="))
    print(console, string.pad_left("café", 6, "*"))
    print(console, string.pad_right("café", 6, "*"))
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "pad diverged between backends");
        // Widths are by character: "café" is 4 chars, so pad to 6 adds two stars
        // (a byte-based width would have added only one).
        assert_eq!(
            compiled,
            vec!["00042", "42...", "hello", "-=-=-ab", "**café", "café**"]
        );
    }

    #[test]
    fn std_list_partition_unzip_backends_agree() {
        // partition splits by a predicate in one pass; unzip is the inverse of
        // zip. Both return tuples of lists, so this also exercises tuple-valued
        // returns from generic std functions across backends.
        let client = r#"
import list

fn main(console: Console):
    let xs = [1, 2, 3, 4, 5, 6]
    let (evens, odds) = list.partition(xs, fn(n: Int): ((n % 2) == 0))
    print(console, int_to_string(list.sum(evens)))
    print(console, int_to_string(list.sum(odds)))
    let pairs = list.zip([10, 20, 30], [1, 2, 3])
    let (a, b) = list.unzip(pairs)
    print(console, int_to_string(list.sum(a)))
    print(console, int_to_string(list.sum(b)))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "partition/unzip diverged between backends");
        assert_eq!(compiled, vec!["12", "9", "60", "6"]);
    }

    #[test]
    fn std_string_strip_backends_agree() {
        // strip_prefix/strip_suffix remove an affix only when it matches,
        // leaving the string untouched otherwise; stripping the whole string
        // yields "". Complements starts_with/ends_with.
        let client = r#"
import string

fn main(console: Console):
    print(console, string.strip_prefix("witchy.lang", "witchy."))
    print(console, string.strip_prefix("witchy.lang", "scala."))
    print(console, string.strip_suffix("main.witchy", ".witchy"))
    print(console, string.strip_suffix("main.rs", ".witchy"))
    print(console, string.strip_prefix("abc", "abc"))
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "strip diverged between backends");
        assert_eq!(compiled, vec!["lang", "witchy.lang", "main", "main.rs", ""]);
    }

    #[test]
    fn large_list_allocation_grows_memory() {
        // Building a 200-element list via `push` allocates ~80KB total (each push
        // copies the whole list, and the bump allocator never frees) — past the
        // initial 64KB page, so the memory must grow. Summing 0..199 verifies no
        // element was corrupted by the growth.
        let src = r#"
fn main() -> Int:
    var out = []
    var i = 0
    while (i < 200):
        out = push(out, i)
        i = (i + 1)
    var total = 0
    for x in out:
        total = (total + x)
    total
"#;
        assert_eq!(run_on_wasm(src), vec!["19900"]); // 199*200/2
    }

    #[test]
    fn large_string_concat_grows_memory() {
        // Concatenating a 400-char string one char at a time allocates ~80KB of
        // intermediate strings — past the initial page — and must grow.
        let src = r#"
fn main() -> Int:
    var s = ""
    var i = 0
    while (i < 400):
        s = (s <> "x")
        i = (i + 1)
    string_length(s)
"#;
        assert_eq!(run_on_wasm(src), vec!["400"]);
    }

    #[test]
    fn compute_runs_on_wasm() {
        assert_eq!(
            run_on_wasm(include_str!("../examples/compute.witchy")),
            vec!["217"]
        );
    }

    #[test]
    fn list_patterns_on_wasm() {
        // Recursive head/tail list processing compiles: `[]` and `[h, ..t]`
        // (the tail is a freshly allocated sublist). sum([10,20,30,40]) = 100.
        let src = r#"
fn sum(xs: List(Int)) -> Int:
    match xs:
        [] -> 0
        [h, ..t] -> (h + sum(t))

fn main() -> Int:
    sum([10, 20, 30, 40])
"#;
        assert_eq!(run_on_wasm(src), vec!["100"]);
    }

    #[test]
    fn list_push_and_concat_on_wasm() {
        // Build a list with `push` in a loop, then `concat` — both allocate new
        // lists at runtime. double_all([1,2,3]) = [2,4,6], ++ [100], summed = 112.
        let src = r#"
fn double_all(xs: List(Int)) -> List(Int):
    var out = []
    for x in xs:
        out = push(out, (x * 2))
    out

fn main() -> Int:
    let ys = concat(double_all([1, 2, 3]), [100])
    (((at(ys, 0) + at(ys, 1)) + at(ys, 2)) + at(ys, 3))
"#;
        assert_eq!(run_on_wasm(src), vec!["112"]);
    }

    #[test]
    fn for_in_over_list_on_wasm() {
        // `for x in list` compiles to a WASM loop; sum a list = 100.
        let src = r#"
fn total(xs: List(Int)) -> Int:
    var sum = 0
    for x in xs:
        sum = (sum + x)
    sum

fn main() -> Int:
    total([10, 20, 30, 40])
"#;
        assert_eq!(run_on_wasm(src), vec!["100"]);
    }

    #[test]
    fn tuple_match_patterns_on_wasm() {
        // Tuple patterns in `match` compile to WASM (no tag; element-wise).
        // classify((3,0))=3, classify((0,5))=5, classify((2,4))=6; sum = 14.
        let src = r#"
fn classify(p: (Int, Int)) -> Int:
    match p:
        (0, 0) -> 0
        (x, 0) -> x
        (0, y) -> y
        (x, y) -> (x + y)

fn main() -> Int:
    ((classify((3, 0)) + classify((0, 5))) + classify((2, 4)))
"#;
        assert_eq!(run_on_wasm(src), vec!["14"]);
    }

    #[test]
    fn tuple_construct_and_destructure_on_wasm() {
        // Multiple-return-value tuples compile to WASM: divmod(17,5) = (3,2),
        // then 3*100 + 2 = 302.
        let src = r#"
fn divmod(a: Int, b: Int) -> (Int, Int):
    ((a / b), (a % b))

fn main() -> Int:
    let (q, r) = divmod(17, 5)
    ((q * 100) + r)
"#;
        assert_eq!(run_on_wasm(src), vec!["302"]);
    }

    #[test]
    fn string_prefix_suffix_on_wasm() {
        // starts_with / ends_with compile to byte-loop helpers.
        // check("html")=2, check("http")=1, check("xml")=0 -> 210.
        let src = r#"
fn check(s: String) -> Int:
    if starts_with(s, "ht"):
        if ends_with(s, "ml"):
            2
        else:
            1
    else:
        0

fn main() -> Int:
    (((check("html") * 100) + (check("http") * 10)) + check("xml"))
"#;
        assert_eq!(run_on_wasm(src), vec!["210"]);
    }

    #[test]
    fn try_operator_runs_on_wasm() {
        // `?` compiles: success unwraps, error early-returns. compute(3,4)=Ok(7),
        // compute(0,9)=Err(99); 7*100 + 99 = 799.
        let src = r#"
type Result:
    Ok(a)
    Err(e)

fn checked(n: Int) -> Result(Int, Int):
    match n:
        0 -> Err(99)
        _ -> Ok(n)

fn compute(a: Int, b: Int) -> Result(Int, Int):
    let x = (checked(a))?
    let y = (checked(b))?
    Ok((x + y))

fn main() -> Int:
    let ok = match compute(3, 4):
        Ok(v) -> v
        Err(e) -> e
    let bad = match compute(0, 9):
        Ok(v) -> v
        Err(e) -> e
    ((ok * 100) + bad)
"#;
        assert_eq!(run_on_wasm(src), vec!["799"]);
    }

    #[test]
    fn early_return_runs_on_wasm() {
        // Guard-clause early returns compile to valid WASM and run.
        // classify(-5) = -1, classify(0) = 0, classify(9) = 1; sum = 0.
        let src = r#"
fn classify(n: Int) -> Int:
    if (n < 0):
        return (0 - 1)
    if (n == 0):
        return 0
    1

fn main() -> Int:
    ((classify((0 - 5)) + classify(0)) + classify(9))
"#;
        assert_eq!(run_on_wasm(src), vec!["0"]);
    }

    #[test]
    fn shapes_runs_on_wasm() {
        assert_eq!(
            run_on_wasm(include_str!("../examples/shapes.witchy")),
            vec!["325"]
        );
    }

    #[test]
    fn nested_record_chained_field_access_on_wasm() {
        // `o.inner.v` — chained access through a nested record — compiles to WASM.
        let src = r#"
type Inner:
    v: Int

type Outer:
    inner: Inner
    tag: Int

fn deep(o: Outer) -> Int:
    (((o).inner).v + (o).tag)

fn main() -> Int:
    let o = Outer(Inner(42), 8)
    deep(o)
"#;
        assert_eq!(run_on_wasm(src), vec!["50"]);
    }

    #[test]
    fn record_call_and_update_results_field_access_on_wasm() {
        // Field access on a `let` bound to a record-returning call / update —
        // exercises return-record and update-result type tracking in codegen.
        let src = r#"
type Point:
    x: Int
    y: Int

fn make(a: Int, b: Int) -> Point:
    Point(a, b)

fn shift(p: Point, dx: Int) -> Point:
    Point(x: ((p).x + dx), ..p)

fn main() -> Int:
    let p = make(3, 4)
    let q = shift(p, 7)
    ((q).x + (q).y)
"#;
        assert_eq!(run_on_wasm(src), vec!["14"]);
    }

    /// Real examples (not toy snippets) compile and run on the WASM backend,
    /// matching the interpreter — a concrete check of codegen breadth.
    #[test]
    fn eval_example_runs_on_wasm() {
        assert_eq!(run_on_wasm(include_str!("../examples/eval.witchy")), vec!["20"]);
    }

    #[test]
    fn records_example_runs_on_wasm() {
        assert_eq!(
            run_on_wasm(include_str!("../examples/records.witchy")),
            vec!["origin.x = 2", "moved = (12, 3)", "manhattan(moved) = 15"]
        );
    }

    #[test]
    fn bank_example_runs_on_wasm() {
        // Records + lists + for-in + Result + `?` together, compiled to WASM.
        assert_eq!(
            run_on_wasm(include_str!("../examples/bank.witchy")),
            vec!["total = 150", "remaining: 90", "error: insufficient funds for bob"]
        );
    }

    #[test]
    fn closures_example_runs_on_wasm() {
        // Higher-order functions + closures, compiled to WASM: apply(square, 9) =
        // 81; twice(+3, 10) = ((10+3)+3) = 16; apply(adder(100), 5) = 105 (the
        // returned closure captures `by = 100`).
        assert_eq!(
            run_on_wasm(include_str!("../examples/closures.witchy")),
            vec!["81", "16", "105"]
        );
    }

    #[test]
    fn record_typed_list_iteration_on_wasm() {
        // `for it in items` where items: List(Record) — the loop var's fields
        // resolve. total([Item(3,2), Item(5,1)]) = 3*2 + 5*1 = 11.
        let src = r#"
type Item:
    price: Int
    qty: Int

fn total(items: List(Item)) -> Int:
    var sum = 0
    for it in items:
        sum = (sum + ((it).price * (it).qty))
    sum

fn main() -> Int:
    total([Item(3, 2), Item(5, 1)])
"#;
        assert_eq!(run_on_wasm(src), vec!["11"]);
    }

    #[test]
    fn record_field_access_and_update_run_on_wasm() {
        // Records — field access *and* update — compile and run on the WASM
        // runtime. shift_x(Point(3,4), 1) = Point(4,4); 4*4 + 4*4 = 32.
        assert_eq!(
            run_on_wasm(include_str!("../examples/record_compiled.witchy")),
            vec!["32"]
        );
    }

    /// The capability thesis at the WASM boundary: without the `print_int` host
    /// function granted, the compiled module imports something that isn't there
    /// and cannot even instantiate.
    #[test]
    fn compiled_program_without_capability_cannot_instantiate() {
        use crate::runtime::{Capabilities, Runtime};
        let module = parser::parse_module(include_str!("../examples/compute.witchy")).expect("parse");
        let wat = codegen::compile_module(&module).expect("compile");
        let mut rt = Runtime::new().expect("runtime");
        let result = rt.spawn(wat.as_bytes(), Capabilities::none(), 4);
        assert!(result.is_err(), "ungranted module must fail to instantiate");
    }

    // A Net capability is an allow-list, and attenuation only ever narrows it.
    // These rejections fire on the allow-list check, before any socket is
    // opened, so the test needs no network. (`run_with` grants the root Net.)
    #[test]
    fn net_capability_cannot_escalate() {
        // connect outside the granted allow-list is denied.
        let connect_denied = r#"
fn main(console: Console, net: Net):
    send_line(connect(net, "evil.test:80"), "x")
"#;
        let e = interpreter::run_with(connect_denied, ".", vec!["allowed.test:80".into()])
            .unwrap_err()
            .to_string();
        assert!(e.contains("not permitted"), "expected a connect denial, got: {e}");

        // restrict to an address not already held is denied (can't widen).
        let restrict_denied = r#"
fn main(console: Console, net: Net):
    send_line(connect(restrict(net, "evil.test:80"), "evil.test:80"), "x")
"#;
        let e = interpreter::run_with(restrict_denied, ".", vec!["allowed.test:80".into()])
            .unwrap_err()
            .to_string();
        assert!(e.contains("not in this Net"), "expected a restrict denial, got: {e}");

        // Attenuation is real: after restricting to one address, a sibling that
        // was in the original grant is no longer reachable.
        let attenuated = r#"
fn main(console: Console, net: Net):
    let narrow = restrict(net, "a.test:80")
    send_line(connect(narrow, "b.test:80"), "x")
"#;
        let e = interpreter::run_with(attenuated, ".", vec!["a.test:80".into(), "b.test:80".into()])
            .unwrap_err()
            .to_string();
        assert!(e.contains("not permitted"), "expected the sibling to be unreachable, got: {e}");
    }

    /// A library imported into a program brings its functions into scope but no
    /// authority: `lib` has no capability parameters, so it can only compute.
    #[test]
    fn imported_library_is_pure_and_confined() {
        let lib = r#"
fn label(n: Int) -> String:
    if (n < 0):
        "neg"
    else:
        "nonneg"
"#;
        let main = r#"
import lib

fn main(console: Console):
    print(console, lib.label((-2)))
    print(console, lib.label(7))
"#;
        let out = interpreter::run_program(&[("lib", lib), ("main", main)], "main")
            .expect("multi-module program runs");
        assert_eq!(out, vec!["neg", "nonneg"]);
    }

    fn assert_actor_compiles(src: &str) {
        assert!(typeck::check_str(src).is_ok(), "{:?}", typeck::check_str(src));
        let module = parser::parse_module(src).expect("parse");
        let actor = module
            .items
            .iter()
            .find_map(|i| match i {
                ast::Item::Actor(a) => Some(a),
                _ => None,
            })
            .expect("an actor");
        let wat = codegen::compile_actor_module(actor).expect("compile");
        Module::new(&Engine::default(), &wat).expect("valid wasm");
    }

    #[test]
    fn actor_handlers_in_impl_block() {
        // Handlers written in a separate `impl Actor { on ... }` block are merged
        // onto the actor (so the actor body holds only state) and compile exactly
        // as inline handlers would.
        let src = r#"
actor Counter:
    console: Console
    var count: Int = 0

impl Counter:
    on Tick():
        count = (count + 1)
        print(console, int_to_string(count))
"#;
        let module = parser::parse_module(src).expect("parse");
        let actor = module
            .items
            .iter()
            .find_map(|i| match i {
                ast::Item::Actor(a) => Some(a),
                _ => None,
            })
            .expect("actor");
        assert_eq!(actor.handlers.len(), 1);
        assert_eq!(actor.handlers[0].message, "Tick");
        // The handler-only impl is consumed by the merge, leaving no impl item.
        assert!(!module.items.iter().any(|i| matches!(i, ast::Item::Impl(_))));
        assert_actor_compiles(src);
    }

    /// Every example must at least compile (parse + link + type-check) and run
    /// to completion through the CLI without an error — whether it prints, just
    /// returns a value, or is a library/actor file with no `main`. Server demos
    /// (`serve_*`) are excluded: they need a `--net` grant and run forever, so
    /// they're covered by the loopback tests instead, not run-to-completion here.
    #[test]
    fn all_examples_run_via_cli() {
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir("examples")
            .expect("examples directory")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("witchy"))
            .filter(|p| {
                !p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("serve_"))
            })
            .collect();
        files.sort();
        assert!(!files.is_empty(), "no examples found");
        for path in files {
            let p = path.to_str().unwrap();
            let result = crate::execute_file(p, Vec::new());
            assert!(result.is_ok(), "example `{p}` failed: {result:?}");
        }
    }

    /// EVERY example — including the server demos that run forever (and so are
    /// excluded from the run-to-completion test above) — must parse, link, and
    /// type-check. Catches type errors the run test can't reach.
    #[test]
    fn all_examples_type_check() {
        let mut any = false;
        for entry in std::fs::read_dir("examples").expect("examples directory") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|s| s.to_str()) != Some("witchy") {
                continue;
            }
            any = true;
            let p = path.to_str().unwrap();
            assert!(
                crate::check_file(p).is_ok(),
                "type-check failed for `{p}`: {:?}",
                crate::check_file(p)
            );
        }
        assert!(any, "no examples found");
    }

    /// Every bundled std module must type-check on its own (linked with its
    /// imports). The interpreter doesn't type-check, so without this a latent
    /// type error in a module no example imports would go unnoticed.
    #[test]
    fn all_std_modules_type_check() {
        let mut any = false;
        for entry in std::fs::read_dir("std").expect("std directory") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|s| s.to_str()) != Some("witchy") {
                continue;
            }
            any = true;
            let p = path.to_str().unwrap();
            assert!(
                crate::check_file(p).is_ok(),
                "type-check failed for `{p}`: {:?}",
                crate::check_file(p)
            );
        }
        assert!(any, "no std modules found");
    }

    #[test]
    fn compute_example_returns_value() {
        assert_eq!(
            crate::execute_file("examples/compute.witchy", Vec::new()).unwrap(),
            vec!["217"]
        );
    }

    #[test]
    fn shapes_example_returns_value() {
        assert_eq!(
            crate::execute_file("examples/shapes.witchy", Vec::new()).unwrap(),
            vec!["325"]
        );
    }

    #[test]
    fn files_example_reads_sandboxed_file() {
        assert_eq!(
            crate::execute_file("examples/files.witchy", Vec::new()).unwrap(),
            vec!["hello from a sandboxed Dir capability"]
        );
    }

    /// The capability-rights showcase: it runs (exercising implicit + explicit
    /// `as` narrowing of a `Dir` to `Dir[Read]`) and its footprint is
    /// verb/transport-precise — the end-to-end demonstration of the feature.
    #[test]
    fn capability_rights_example_runs_and_audits() {
        assert_eq!(
            crate::execute_file("examples/capability_rights.witchy", Vec::new()).unwrap(),
            vec![
                "implicit: hello from a sandboxed Dir capability",
                "explicit: hello from a sandboxed Dir capability",
            ]
        );
        let src = std::fs::read_to_string("examples/capability_rights.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        let shown = |name: &str| {
            let e = fp.entries.iter().find(|e| e.name == name).expect("entry");
            crate::capabilities::show_caps(&e.capabilities)
        };
        assert_eq!(shown("load"), "Dir[Read]");
        assert_eq!(shown("fetch"), "Net[Connect, Tcp]");
        assert_eq!(shown("serve"), "Net[Listen]");
    }

    /// `pascal` is an infinite generator whose state is a `List(Int)` row — each
    /// `yield` emits a row, the next built from it. Demonstrates `gen fn` carrying
    /// non-scalar state; agrees on both backends.
    #[test]
    fn pascal_generator_example_agrees_on_both_backends() {
        let client = std::fs::read_to_string("examples/pascal.witchy").unwrap();
        let sources = [
            ("iter", crate::bundled_module("iter").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client.as_str()),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "pascal diverged");
        assert_eq!(
            compiled,
            vec!["1", "1 1", "1 2 1", "1 3 3 1", "1 4 6 4 1", "1 5 10 10 5 1"]
        );
    }

    /// `split_first` + `drop_while` let a user write their own iterator
    /// transforms — here `dedup` (drop consecutive duplicates), composed with
    /// `unfold`. Must agree on both backends.
    #[test]
    fn std_iter_split_first_dedup_backends_agree() {
        let client = std::fs::read_to_string("examples/dedup.witchy").unwrap();
        let sources = [
            ("iter", crate::bundled_module("iter").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client.as_str()),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "dedup diverged");
        assert_eq!(compiled, vec!["1 2 3 2 4".to_string()]);
    }

    /// The `std/iter` adapters `enumerate`/`zip`/`chain`/`flat_map`/`for_each`
    /// (plus `func.first`/`second` for the pairs they produce) must agree on both
    /// backends — they compose lazily over finite and infinite iterators.
    #[test]
    fn std_iter_more_adapters_backends_agree() {
        let client = r#"
import iter
import func
import string
fn main(console: Console):
    var es = []
    for p in iter.collect(iter.enumerate(iter.from_list(["a", "b", "c"]))):
        es = push(es, int_to_string(func.first(p)) <> func.second(p))
    print(console, string.join(es, " "))
    print(console, int_to_string(iter.count(iter.zip(iter.count_from(1), iter.from_list([0, 0, 0])))))
    print(console, int_to_string(iter.sum(iter.chain(iter.range(0, 4), iter.range(10, 13)))))
    print(console, int_to_string(iter.sum(iter.flat_map(iter.range(1, 4), fn(n: Int): iter.from_list([n, n])))))
    iter.for_each(iter.take(iter.count_from(100), 3), fn(n: Int): print(console, int_to_string(n)))
"#;
        let sources = [
            ("iter", crate::bundled_module("iter").unwrap()),
            ("func", crate::bundled_module("func").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "iter adapters diverged");
        assert_eq!(compiled, vec!["0a 1b 2c", "3", "39", "12", "100", "101", "102"]);
    }

    /// `gen fn` / `yield` (lowered by `crate::generators` to `std/iter`): an
    /// imperative generator that yields a sequence becomes a lazy iterator. The
    /// `generators` example (Fibonacci + Collatz, incl. an infinite generator and
    /// a branch inside a loop) must agree on both backends.
    #[test]
    fn gen_yield_generators_example_agrees_on_both_backends() {
        let client = std::fs::read_to_string("examples/generators.witchy").unwrap();
        let sources = [
            ("iter", crate::bundled_module("iter").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client.as_str()),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "generators diverged");
        assert_eq!(
            compiled,
            vec![
                "fib[0..10): 0, 1, 1, 2, 3, 5, 8, 13, 21, 34".to_string(),
                "collatz(6): 6, 3, 10, 5, 16, 8, 4, 2, 1".to_string(),
                "collatz(27) length: 112".to_string(),
            ]
        );
    }

    /// A `gen fn` lowers to a `__gen_*` helper (yield -> counter + early return)
    /// plus a wrapper calling `iter.from_gen`, and `import iter` is injected.
    #[test]
    fn gen_fn_lowers_to_helper_and_wrapper() {
        let m = parser::parse_module("gen fn nums() -> Iter(Int):\n    yield 1\n    yield 2\n")
            .expect("parse");
        let lowered = crate::generators::lower(m);
        let fn_names: Vec<&str> = lowered
            .items
            .iter()
            .filter_map(|it| match it {
                crate::ast::Item::Function(f) => Some(f.name.as_str()),
                _ => None,
            })
            .collect();
        assert!(fn_names.contains(&"__gen_nums"), "missing helper: {fn_names:?}");
        assert!(fn_names.contains(&"nums"), "missing wrapper: {fn_names:?}");
        assert!(lowered.imports.iter().any(|m| m == "iter"), "iter not imported");
        // No `gen fn` or `yield` survives lowering.
        assert!(lowered.items.iter().all(|it| !matches!(it, crate::ast::Item::Function(f) if f.is_gen)));
    }

    /// `std/iter` is the lazy pull-based iterator module (witchy's answer to
    /// Rust's Iterator). Lazy `map`/`filter`/`take_while` over an *infinite*
    /// `count_from`, plus `find`/`sum`/`collect`/`count` consumers, must agree on
    /// both backends — closures-in-ADTs + recursion compile to WASM.
    #[test]
    fn std_iter_lazy_adapters_backends_agree() {
        let client = r#"
import iter
fn main(console: Console):
    // squares of 1.. while < 100, kept odd, summed: 1+9+25+49+81 = 165
    let sq = iter.map(iter.count_from(1), fn(n: Int): n * n)
    let small = iter.take_while(sq, fn(s: Int): s < 100)
    print(console, int_to_string(iter.sum(iter.filter(small, fn(s: Int): s % 2 == 1))))
    // first multiple of 7 above 50, from an infinite iterator
    match iter.find(iter.count_from(51), fn(n: Int): n % 7 == 0):
        Some(n) -> print(console, int_to_string(n))
        None -> print(console, "none")
    // a finite range, doubled and collected
    print(console, int_to_string(iter.count(iter.range(0, 5))))
    for v in iter.collect(iter.map(iter.range(0, 3), fn(n: Int): n * 10)):
        print(console, int_to_string(v))
"#;
        let sources = [("iter", crate::bundled_module("iter").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std/iter diverged");
        assert_eq!(compiled, vec!["165", "56", "5", "0", "10", "20"]);
    }

    /// `lazy_fib` builds an *infinite* Fibonacci iterator with `iter.unfold` and
    /// bounds it with take / take_while / find — the canonical lazy-generator
    /// demo, agreeing on both backends.
    #[test]
    fn lazy_fib_example_agrees_on_both_backends() {
        let client = std::fs::read_to_string("examples/lazy_fib.witchy").unwrap();
        let sources = [
            ("iter", crate::bundled_module("iter").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client.as_str()),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "lazy_fib diverged");
        assert_eq!(
            compiled,
            vec![
                "first 10: 0, 1, 1, 2, 3, 5, 8, 13, 21, 34".to_string(),
                "even fib sum < 1000: 798".to_string(),
                "first fib > 1000: 1597".to_string(),
            ]
        );
    }

    /// `largest` reproduces the generic function from The Rust Programming
    /// Language ch. 10: a `where a: Ord` bound finds the biggest element of a
    /// list, for `Int` and for a user `Version` type with an `Ord` impl (the
    /// trait's derived `greater` dispatches correctly through monomorphization).
    #[test]
    fn largest_example_agrees_on_both_backends() {
        let client = std::fs::read_to_string("examples/largest.witchy").unwrap();
        let sources = [("ord", crate::bundled_module("ord").unwrap()), ("main", client.as_str())];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "largest diverged");
        assert_eq!(
            compiled,
            vec!["largest number: 100".to_string(), "latest version: 2.0".to_string()]
        );
    }

    /// `higher_order_sum` reproduces Rust by Example's "sum of squared odd numbers
    /// under 1000" — an imperative range loop and a functional `std/list` pipeline
    /// (map / take_while / filter / sum) that must agree, on both backends.
    #[test]
    fn higher_order_sum_example_agrees_on_both_backends() {
        let client = std::fs::read_to_string("examples/higher_order_sum.witchy").unwrap();
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client.as_str())];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "higher_order_sum diverged");
        assert_eq!(compiled, vec!["imperative: 5456".to_string(), "functional: 5456".to_string()]);
    }

    /// `minigrep` is the CLI search tool from The Rust Programming Language ch. 12,
    /// reproduced in witchy: it takes a query and a file path as args, reads the
    /// file with a `Dir[Read]` capability, and prints the matching lines. Missing
    /// args print usage and exit 1 (the conventional process exit code).
    #[test]
    fn minigrep_example_searches_a_file_like_the_rust_book() {
        let (out, code) = crate::execute_file_exit(
            "examples/minigrep.witchy",
            Vec::new(),
            vec!["nobody".into(), "examples/data/poem.txt".into()],
            None,
        )
        .unwrap();
        assert_eq!(code, 0);
        assert_eq!(
            out,
            vec!["I'm nobody! Who are you?".to_string(), "Are you nobody, too?".to_string()]
        );
        // No args: usage message and a non-zero exit code.
        let (out, code) =
            crate::execute_file_exit("examples/minigrep.witchy", Vec::new(), Vec::new(), None)
                .unwrap();
        assert_eq!(code, 1);
        assert_eq!(out, vec!["usage: minigrep <query> <file>".to_string()]);
    }

    /// `caps_audit` is a capability auditor written *in witchy*: it reads a source
    /// file (`Dir[Read]`), computes its footprint via `compiler.footprint`, parses
    /// the JSON with `std/json`, and prints the total — a self-hosted slice of
    /// `witchy caps`, proving the toolchain is usable from within the language.
    #[test]
    fn caps_audit_example_audits_a_rune_in_witchy() {
        assert_eq!(
            crate::execute_file("examples/caps_audit.witchy", Vec::new()).unwrap(),
            vec!["examples/data/sample_rune.witchy demands: Dir[Read], Net[Connect]"]
        );
        // The auditor itself only reads files and prints — provably no writes/net.
        let src = std::fs::read_to_string("examples/caps_audit.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console, Dir[Read]");
    }

    /// `caps_guard` is the supply-chain gate written *in witchy*: it reads two
    /// versions of a rune, asks `compiler.diff` whether the new one widens the
    /// footprint, prints a BLOCK/OK verdict, AND exits non-zero on a widening
    /// (the sample upgrade adds `Listen`, so it BLOCKs and exits 2 — wireable into
    /// CI). The whole gate is self-hosted.
    #[test]
    fn caps_guard_example_blocks_a_widening_in_witchy() {
        let (output, code) =
            crate::execute_file_exit("examples/caps_guard.witchy", Vec::new(), Vec::new(), None)
                .unwrap();
        assert_eq!(output, vec!["BLOCK: upgrade widens authority by Net[Listen]"]);
        assert_eq!(code, 2, "a widening must exit 2");
        let src = std::fs::read_to_string("examples/caps_guard.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console, Dir[Read]");
    }

    /// `coven_check` is the package manager's `check_declared`, self-hosted: it
    /// reads a rune's `witchy.toml` (`std/toml`) and its source, asks the compiler
    /// what the code demands (`compiler.footprint`), and verifies the manifest's
    /// `[capabilities]` admits every demanded cap *rights-precisely*. The sample
    /// manifest admits `Net[Connect]`, but the code demands full `Net` (it also
    /// listens), so it flags the under-declaration and exits 1 even though the
    /// `Net` *kind* is declared — the case a kind-level check would miss.
    #[test]
    fn coven_check_example_flags_under_declared_manifest_in_witchy() {
        let (output, code) =
            crate::execute_file_exit("examples/coven_check.witchy", Vec::new(), Vec::new(), None)
                .unwrap();
        assert_eq!(
            output,
            vec!["UNDER-DECLARED: code demands Net not admitted by [capabilities]"]
        );
        assert_eq!(code, 1, "an under-declared manifest must exit 1");
        // The checker itself only reads files and prints — provably no writes/net.
        let src = std::fs::read_to_string("examples/coven_check.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console, Dir[Read]");
    }

    /// `main -> Int` sets the process exit code (C/Go/Rust convention) and is
    /// *not* printed; `main` returning Nil exits 0 and shows its `print` output.
    #[test]
    fn main_int_return_is_the_process_exit_code() {
        let run = |src: &str| {
            let m = parser::parse_module(src).expect("parse");
            let l = crate::linker::link(vec![("main".into(), m)], "main").expect("link");
            interpreter::run_module_exit(l, ".", Vec::new(), Vec::new(), None).expect("run")
        };
        let (out, code) = run("fn main() -> Int:\n    7\n");
        assert!(out.is_empty(), "an Int return must not be printed, got {out:?}");
        assert_eq!(code, 7);
        let (out, code) = run("fn main(console: Console):\n    print(console, \"hi\")\n");
        assert_eq!(out, vec!["hi"]);
        assert_eq!(code, 0);
    }

    #[test]
    fn dir_write_is_confined_to_the_subtree() {
        let tmp = std::env::temp_dir().join("witchy_dir_write_test");
        std::fs::create_dir_all(&tmp).unwrap();
        let run = |src: &str| {
            let mods = vec![("main".to_string(), parser::parse_module(src).expect("parse"))];
            let linked = crate::linker::link(mods, "main").expect("link");
            interpreter::run_module(linked, &tmp, Vec::new())
        };
        // Write then read back, within the confined Dir.
        let out = run("fn main(console: Console, root: Dir):\n    write(root, \"out.txt\", \"hi\")\n    print(console, read(root, \"out.txt\"))\n")
            .expect("run");
        assert_eq!(out, vec!["hi"]);
        assert_eq!(std::fs::read_to_string(tmp.join("out.txt")).unwrap(), "hi");
        // A `..` write is refused — the capability can't escape its subtree.
        assert!(run("fn main(console: Console, root: Dir):\n    write(root, \"../escape.txt\", \"x\")\n").is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `list` (enumerate, sorted) and `make_dir` (create a confined subdir) — the
    /// filesystem ops a package store/registry needs. `list` needs `Read`,
    /// `make_dir` needs `Write`, and both stay confined to the capability's subtree.
    #[test]
    fn dir_list_and_make_dir_work_and_are_rights_checked() {
        let tmp = std::env::temp_dir().join("witchy_dir_list_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("store")).unwrap();
        std::fs::write(tmp.join("store/bravo"), "b").unwrap();
        std::fs::write(tmp.join("store/alpha"), "a").unwrap();
        let run = |src: &str| {
            let mods = vec![("main".to_string(), parser::parse_module(src).expect("parse"))];
            let linked = crate::linker::link(mods, "main").expect("link");
            interpreter::run_module(linked, &tmp, Vec::new())
        };
        // `list` enumerates a subdir's entries in sorted (deterministic) order.
        let out = run("import string\nfn main(console: Console, root: Dir):\n    print(console, string.join(list(subdir(root, \"store\")), \",\"))\n")
            .expect("run");
        assert_eq!(out, vec!["alpha,bravo"]);
        // `make_dir` creates a confined subdirectory.
        run("fn main(console: Console, root: Dir):\n    make_dir(root, \"fresh\")\n").expect("run");
        assert!(tmp.join("fresh").is_dir(), "make_dir should have created the directory");
        // Confinement: a `..` make_dir is refused.
        assert!(run("fn main(console: Console, root: Dir):\n    make_dir(root, \"../escaped\")\n").is_err());
        assert!(!tmp.parent().unwrap().join("escaped").exists(), "make_dir must not escape the subtree");

        // Rights: `list` needs Read, `make_dir` needs Write.
        assert!(typeck::check_str("fn main(c: Console, d: Dir[Write]):\n    let n = list(d)\n").is_err());
        assert!(typeck::check_str("fn main(c: Console, d: Dir[Read]):\n    make_dir(d, \"x\")\n").is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn dir_write_refuses_a_symlink_leaf() {
        // A pre-existing symlink in the subtree must not let a write escape it.
        let base = std::env::temp_dir().join("witchy_dir_symlink_test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sandbox")).unwrap();
        std::fs::write(base.join("secret.txt"), "ORIGINAL").unwrap();
        std::os::unix::fs::symlink("../secret.txt", base.join("sandbox/link.txt")).unwrap();

        let mods = vec![(
            "main".to_string(),
            parser::parse_module(
                "fn main(console: Console, root: Dir):\n    write(subdir(root, \"sandbox\"), \"link.txt\", \"PWNED\")\n",
            )
            .expect("parse"),
        )];
        let linked = crate::linker::link(mods, "main").expect("link");
        assert!(interpreter::run_module(linked, &base, Vec::new()).is_err());
        // The symlink target outside the subtree is untouched.
        assert_eq!(std::fs::read_to_string(base.join("secret.txt")).unwrap(), "ORIGINAL");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Rights-parameterized `Dir`: the right-set in the type statically gates the
    /// ops. A `Dir[Read]` structurally cannot `write`; bare `Dir` is the full set
    /// (back-compat); `read_only`/`write_only` are monotone attenuations that the
    /// checker enforces (you can only keep a right you already hold).
    #[test]
    fn dir_rights_are_statically_enforced() {
        let ok = |src: &str| {
            assert!(
                crate::typeck::check_str(src).is_ok(),
                "expected ok, got: {:?}",
                crate::typeck::check_str(src)
            );
        };
        let err = |src: &str, needle: &str| {
            let e = crate::typeck::check_str(src).expect_err("expected a type error");
            assert!(e.contains(needle), "error `{e}` should mention `{needle}`");
        };

        // Bare `Dir` carries the full right-set: reads and writes both type-check.
        ok("fn use_both(d: Dir):\n    write(d, \"o\", read(d, \"i\"))\nfn main(c: Console, root: Dir):\n    use_both(root)\n");
        // `Dir[Read]` cannot write — a compile-time error.
        err(
            "fn save(d: Dir[Read]):\n    write(d, \"o\", \"x\")\nfn main(c: Console, root: Dir):\n    save(root)\n",
            "`write` needs `Write`",
        );
        // `Dir[Write]` cannot read.
        err(
            "fn load(d: Dir[Write]):\n    let s = read(d, \"i\")\nfn main(c: Console, root: Dir):\n    load(root)\n",
            "`read` needs `Read`",
        );
        // `as Dir[Read]` narrows; a later write through it is rejected.
        err(
            "fn f(d: Dir):\n    let r = d as Dir[Read]\n    write(r, \"o\", \"x\")\nfn main(c: Console, root: Dir):\n    f(root)\n",
            "`write` needs `Write`",
        );
        // `as` cannot resurrect a `Write` the capability never had (not a subset).
        err(
            "fn f(d: Dir[Read]):\n    let w = d as Dir[Write]\nfn main(c: Console, root: Dir):\n    f(root)\n",
            "`as` can only drop rights",
        );
        // `Dir[Read, Write]` is equivalent to bare `Dir` — both verbs allowed.
        ok("fn f(d: Dir[Read, Write]):\n    write(d, \"o\", read(d, \"i\"))\nfn main(c: Console, root: Dir):\n    f(root)\n");
    }

    /// `as` narrowing is the identity at runtime (rights live only in the type),
    /// so a narrowed handle still reads the same confined subtree.
    #[test]
    fn as_narrowing_is_identity_at_runtime() {
        let tmp = std::env::temp_dir().join("witchy_dir_as_narrow_test");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("in.txt"), "narrowed").unwrap();
        let src = "fn main(console: Console, root: Dir):\n    let r = root as Dir[Read]\n    print(console, read(r, \"in.txt\"))\n";
        let mods = vec![("main".to_string(), parser::parse_module(src).expect("parse"))];
        let linked = crate::linker::link(mods, "main").expect("link");
        let out = interpreter::run_module(linked, &tmp, Vec::new()).expect("run");
        assert_eq!(out, vec!["narrowed"]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The `as` ascription narrows a capability to a subset of its rights, and is
    /// the single native mechanism for it (replacing the per-right `_only`
    /// builtins). It can only *drop* rights — never widen or cross capabilities.
    #[test]
    fn as_ascription_narrows_to_subsets_only() {
        let ok = |src: &str| {
            assert!(
                crate::typeck::check_str(src).is_ok(),
                "expected ok, got: {:?}",
                crate::typeck::check_str(src)
            );
        };
        let err = |src: &str, needle: &str| {
            let e = crate::typeck::check_str(src).expect_err("expected a type error");
            assert!(e.contains(needle), "error `{e}` should mention `{needle}`");
        };

        // Narrowing along each axis, and an idempotent re-ascription, type-check.
        ok("fn main(c: Console, net: Net, root: Dir):\n    let a = net as Net[Connect]\n    let b = net as Net[Listen, Tcp]\n    let d = root as Dir[Read]\n    let e = (net as Net[Connect]) as Net[Connect]\n");
        // Re-widening (`Net[Connect]` back to full `Net`) is rejected.
        err(
            "fn main(c: Console, net: Net):\n    let w = (net as Net[Connect]) as Net\n",
            "`as` can only drop rights",
        );
        // `as` cannot cross capabilities (a `Net` is not a `Dir`).
        err(
            "fn main(c: Console, net: Net):\n    let x = net as Dir[Read]\n",
            "cannot ascribe",
        );
        // The retired narrowing builtins are gone — calling one is unknown.
        err(
            "fn main(c: Console, net: Net):\n    let x = connect_only(net)\n",
            "unknown function `connect_only`",
        );
    }

    /// Implicit directional narrowing wherever a value flows into a capability-
    /// typed slot — call arguments, return types, constructor fields, and actor
    /// spawn fields: a broader capability satisfies a narrower one (a full `Net`
    /// flows into a `Net[Connect]`) without an explicit `as`. The callee stays
    /// type-bounded to its declared rights, so widening is rejected everywhere.
    #[test]
    fn implicit_narrowing_at_call_boundaries() {
        let ok = |src: &str| {
            assert!(
                crate::typeck::check_str(src).is_ok(),
                "expected ok, got: {:?}",
                crate::typeck::check_str(src)
            );
        };
        let err = |src: &str, needle: &str| {
            let e = crate::typeck::check_str(src).expect_err("expected a type error");
            assert!(e.contains(needle), "error `{e}` should mention `{needle}`");
        };

        // A full `Net`/`Dir` coerces into a narrowed parameter — no `as` needed.
        ok("fn fetch(n: Net[Connect]) -> Socket:\n    connect(n, \"a:1\")\nfn main(c: Console, net: Net):\n    let s = fetch(net)\n");
        ok("fn dial(n: Net[Connect, Tcp]) -> Socket:\n    connect(n, \"a:1\")\nfn main(c: Console, net: Net):\n    let s = dial(net)\n");
        ok("fn load(d: Dir[Read]) -> String:\n    read(d, \"f\")\nfn main(c: Console, root: Dir):\n    let x = load(root)\n");
        // The type ceiling holds: a `Net[Connect]` cannot be re-widened to satisfy
        // a full-`Net` parameter (soundness — no laundering authority back up).
        err(
            "fn g(m: Net):\n    let l = listen(m, \"b:2\")\nfn f(n: Net[Connect]):\n    g(n)\nfn main(c: Console, net: Net):\n    f(net)\n",
            "expected `Net`, found `Net[Connect]`",
        );
        // A too-narrow argument is still rejected (Connect cannot satisfy Listen).
        err(
            "fn serve(n: Net[Listen]):\n    let l = listen(n, \"b:2\")\nfn main(c: Console, net: Net):\n    serve(net as Net[Connect])\n",
            "expected `Net[Listen]`, found `Net[Connect]`",
        );

        // The same directional narrowing holds wherever a value flows into a
        // capability-typed slot, not just call arguments:
        // (a) a return type — return a full `Net` where `Net[Connect]` is declared,
        ok("fn client(net: Net) -> Net[Connect]:\n    net\nfn main(c: Console, net: Net):\n    let s = connect(client(net), \"a:1\")\n");
        // (b) a constructor field that holds a narrowed capability.
        ok("type Client:\n    Client(Net[Connect])\nfn main(c: Console, net: Net):\n    let x = Client(net)\n");
        // (c) an actor field granted at spawn.
        ok("actor Cl:\n    n: Net[Connect]\nimpl Cl:\n    on Go(a: String):\n        let s = connect(n, a)\nfn main(c: Console, net: Net):\n    let x = spawn Cl(net)\n");
        // Both still reject *widening* (the type ceiling holds at every position).
        err(
            "fn bad(n: Net[Connect]) -> Net:\n    n\nfn main(c: Console, net: Net):\n    bad(net as Net[Connect])\n",
            "expected `Net`, found `Net[Connect]`",
        );
        err(
            "type Server:\n    Server(Net)\nfn make(n: Net[Connect]) -> Server:\n    Server(n)\nfn main(c: Console, net: Net):\n    make(net as Net[Connect])\n",
            "expected `Net`, found `Net[Connect]`",
        );
    }

    /// Rights-parameterized `Net`: the verb-set in the type distinguishes a client
    /// from a server. `Net[Connect]` cannot `listen`; `Net[Listen]` cannot
    /// `connect`; bare `Net` is the full set (back-compat). Narrowing is done with
    /// the `as` ascription, which can only drop rights.
    #[test]
    fn net_verbs_are_statically_enforced() {
        let ok = |src: &str| {
            assert!(
                crate::typeck::check_str(src).is_ok(),
                "expected ok, got: {:?}",
                crate::typeck::check_str(src)
            );
        };
        let err = |src: &str, needle: &str| {
            let e = crate::typeck::check_str(src).expect_err("expected a type error");
            assert!(e.contains(needle), "error `{e}` should mention `{needle}`");
        };

        // Bare `Net` grants both verbs.
        ok("fn f(n: Net):\n    let s = connect(n, \"a:1\")\n    let l = listen(n, \"b:2\")\nfn main(c: Console, net: Net):\n    f(net)\n");
        // `Net[Connect]` is a client — it cannot listen.
        err(
            "fn f(n: Net[Connect]):\n    let l = listen(n, \"b:2\")\nfn main(c: Console, net: Net):\n    f(net)\n",
            "`listen` needs `Listen`",
        );
        // `Net[Listen]` is a server — it cannot dial out.
        err(
            "fn f(n: Net[Listen]):\n    let s = connect(n, \"a:1\")\nfn main(c: Console, net: Net):\n    f(net)\n",
            "`connect` needs `Connect`",
        );
        // `as Net[Connect]` narrows; listening through it is rejected.
        err(
            "fn f(n: Net):\n    let c = n as Net[Connect]\n    let l = listen(c, \"b:2\")\nfn main(c: Console, net: Net):\n    f(net)\n",
            "`listen` needs `Listen`",
        );
        // `as` cannot resurrect a `Connect` the capability never had (not a subset).
        err(
            "fn f(n: Net[Listen]):\n    let c = n as Net[Connect]\nfn main(c: Console, net: Net):\n    f(net)\n",
            "`as` can only drop rights",
        );
        // `restrict` is verb-neutral: it preserves the verb-set it was given.
        err(
            "fn f(n: Net[Connect]):\n    let r = restrict(n, \"a:1\")\n    let l = listen(r, \"b:2\")\nfn main(c: Console, net: Net):\n    f(net)\n",
            "`listen` needs `Listen`",
        );
    }

    /// The `Net` transport axis: only `Tcp` is implemented, so `connect`/`listen`
    /// require it; `Udp`/`Uds` are type-level markers that keep the taxonomy
    /// expressible. Each axis defaults to full independently (`Net[Connect]` keeps
    /// all transports). Narrowing the transport axis is done with `as`.
    #[test]
    fn net_transport_is_statically_enforced() {
        let ok = |src: &str| {
            assert!(
                crate::typeck::check_str(src).is_ok(),
                "expected ok, got: {:?}",
                crate::typeck::check_str(src)
            );
        };
        let err = |src: &str, needle: &str| {
            let e = crate::typeck::check_str(src).expect_err("expected a type error");
            assert!(e.contains(needle), "error `{e}` should mention `{needle}`");
        };

        // `Net[Connect]` keeps all transports (incl. Tcp), so connect works.
        ok("fn f(n: Net[Connect]):\n    let s = connect(n, \"a:1\")\nfn main(c: Console, net: Net):\n    f(net as Net[Connect])\n");
        // A transport narrowed away from Tcp cannot drive a (TCP-only) connect.
        err(
            "fn f(n: Net[Connect, Udp]):\n    let s = connect(n, \"a:1\")\nfn main(c: Console, net: Net):\n    f(net as Net[Connect, Udp])\n",
            "only implemented over `Tcp`",
        );
        err(
            "fn f(n: Net[Listen, Uds]):\n    let l = listen(n, \"a:1\")\nfn main(c: Console, net: Net):\n    f(net as Net[Listen, Uds])\n",
            "only implemented over `Tcp`",
        );
        // `as Net[Connect, Tcp]` narrows both axes; a TCP connect through the
        // result type-checks end to end.
        ok("fn dial(n: Net[Connect, Tcp]) -> Socket:\n    connect(n, \"a:1\")\nfn main(c: Console, net: Net):\n    let s = dial(net as Net[Connect, Tcp])\n");
        // You cannot keep a transport the capability does not hold (not a subset).
        err(
            "fn f(n: Net[Connect, Tcp]):\n    let u = n as Net[Connect, Udp]\nfn main(c: Console, net: Net):\n    f(net as Net[Connect, Tcp])\n",
            "`as` can only drop rights",
        );
    }

    /// `import list` resolves to the bundled standard library (no local file),
    /// links, type-checks, and runs end to end through the CLI.
    #[test]
    fn std_library_resolves_and_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/std_demo.witchy", Vec::new()).unwrap(),
            vec!["30", "3"]
        );
    }

    /// Sorting with a comparator closure, end to end through the bundled std.
    #[test]
    fn sort_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/sort.witchy", Vec::new()).unwrap(),
            vec!["1,1,3,4,5", "5,4,3,1,1"]
        );
    }

    /// The bundled `math` module resolves and computes via the CLI.
    #[test]
    fn math_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/math_demo.witchy", Vec::new()).unwrap(),
            vec!["7", "5", "10", "1024", "12"]
        );
    }

    /// Float math: the `sqrt` builtin and the `math` module's Float helpers.
    #[test]
    fn floats_run_via_cli() {
        assert_eq!(
            crate::execute_file("examples/floats.witchy", Vec::new()).unwrap(),
            vec!["4", "3.5", "5", "1"]
        );
    }

    /// The list module's search/slice helpers via the CLI.
    #[test]
    fn list_more_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/list_more.witchy", Vec::new()).unwrap(),
            vec!["true", "3", "-1", "20", "30"]
        );
    }

    /// The list-combinator pipeline example runs via the CLI (interpreter); a
    /// companion compiled test (`list_pipeline_example_runs_on_wasm`) asserts the
    /// same output through the WASM backend.
    #[test]
    fn list_pipeline_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/list_pipeline.witchy", Vec::new()).unwrap(),
            vec!["233", "2 8", "735"]
        );
    }

    /// `zip`/`enumerate` and tuple destructuring in a loop, via the CLI.
    #[test]
    fn zip_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/zip.witchy", Vec::new()).unwrap(),
            vec!["0:alice 1:bob 2:carol", "alice=30 bob=25 carol=40"]
        );
    }

    /// `any`/`all` predicate combinators via the CLI.
    #[test]
    fn predicates_run_via_cli() {
        assert_eq!(
            crate::execute_file("examples/predicates.witchy", Vec::new()).unwrap(),
            vec!["true", "true", "false", "false"]
        );
    }

    /// `all` is vacuously true on the empty list; `any` is false.
    #[test]
    fn any_all_empty_list_edge_cases() {
        let client = r#"
import list

fn main(console: Console):
    let empty = list.filter([1], fn(n: Int): (n > 100))
    print(console, to_string(list.all(empty, fn(n: Int): (n > 0))))
    print(console, to_string(list.any(empty, fn(n: Int): (n > 0))))
"#;
        let out = interpreter::run_program(
            &[("list", crate::bundled_module("list").unwrap()), ("main", client)],
            "main",
        )
        .expect("predicates program runs");
        assert_eq!(out, vec!["true", "false"]);
    }

    /// `zip` is generic and stops at the shorter list.
    #[test]
    fn zip_is_generic_and_truncates() {
        let client = r#"
import list

fn main(console: Console):
    let ps = list.zip([1, 2, 3], ["a", "b"])
    print(console, int_to_string(length(ps)))
    let first = at(ps, 0)
    let (n, s) = first
    print(console, (int_to_string(n) <> s))
"#;
        let out = interpreter::run_program(
            &[("list", crate::bundled_module("list").unwrap()), ("main", client)],
            "main",
        )
        .expect("zip program runs");
        assert_eq!(out, vec!["2", "1a"]);
    }

    /// `contains`/`index_of` are generic — they work on Strings too (by value).
    #[test]
    fn list_contains_is_generic_over_element_type() {
        let client = r#"
import list

fn main(console: Console):
    let words = ["a", "bb", "ccc"]
    print(console, to_string(list.contains(words, "bb")))
    print(console, int_to_string(list.index_of(words, "ccc")))
"#;
        let out = interpreter::run_program(
            &[("list", crate::bundled_module("list").unwrap()), ("main", client)],
            "main",
        )
        .expect("list program runs");
        assert_eq!(out, vec!["true", "2"]);
    }

    /// The bundled `option` module (type + helpers) resolves via the CLI.
    #[test]
    fn option_module_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/option_std.witchy", Vec::new()).unwrap(),
            vec!["10", "-1"]
        );
    }

    /// The bundled `result` module supplies the type `?` recognizes, plus
    /// helpers, when linked against a client.
    #[test]
    fn result_module_links_with_try_and_helpers() {
        let client = r#"
import result

fn checked_div(a: Int, b: Int) -> Result(Int, String):
    match b:
        0 -> Err("zero")
        _ -> Ok((a / b))

fn compute(x: Int, y: Int) -> Result(Int, String):
    let q = (checked_div(x, y))?
    Ok((q + 1))

fn main(console: Console):
    print(console, int_to_string(result.unwrap_or(compute(10, 2), (0 - 1))))
    print(console, int_to_string(result.unwrap_or(compute(10, 0), (0 - 1))))
    print(console, to_string(result.is_ok(compute(10, 0))))
"#;
        let out = interpreter::run_program(
            &[("result", crate::bundled_module("result").unwrap()), ("main", client)],
            "main",
        )
        .expect("result module program runs");
        assert_eq!(out, vec!["6", "-1", "false"]);
    }

    /// String builtins + the bundled `list`/`string` modules end to end.
    #[test]
    fn text_processing_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/text.witchy", Vec::new()).unwrap(),
            vec!["ALICE | BOB | CAROL", "===", "alice,***,carol"]
        );
    }

    /// The bundled `list` module type-checks and links against a client program.
    #[test]
    fn bundled_list_module_links() {
        let client = r#"
import list

fn main(console: Console):
    let xs = list.map(list.range(4), fn(n: Int): (n + 1))
    print(console, int_to_string(list.fold(xs, 0, fn(a: Int, b: Int): (a + b))))
"#;
        let out = interpreter::run_program(
            &[("list", crate::bundled_module("list").unwrap()), ("main", client)],
            "main",
        )
        .expect("std list program runs");
        assert_eq!(out, vec!["10"]); // (1+2+3+4)
    }

    #[test]
    fn hello_example() {
        assert_eq!(
            interp(include_str!("../examples/hello.witchy")),
            vec!["hello, witchy", "8 doubled is 16", "negative"]
        );
    }

    #[test]
    fn mutate_example() {
        assert_eq!(
            interp(include_str!("../examples/mutate.witchy")),
            vec!["bumped to 3"]
        );
    }

    #[test]
    fn ownership_example() {
        assert_eq!(
            interp(include_str!("../examples/ownership.witchy")),
            vec!["[witchy]"]
        );
    }

    #[test]
    fn string_interpolation_backends_agree() {
        // `${expr}` desugars to `<> to_string(expr) <>`, so interpolation works
        // in both backends: String pass-through, Int/Bool via to_string, embedded
        // calls/arithmetic, `\$` for a literal `$`, and adjacent interpolations.
        let src = r#"
fn main(console: Console):
    let name = "witchy"
    let age = 3
    print(console, "hi ${name}, age ${age}")
    print(console, "sum: ${int_to_string(age + 10)}")
    print(console, "flag ${age > 1}")
    print(console, "literal \${x} stays")
    print(console, "${name}${name}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(
            run_on_wasm(src),
            vec![
                "hi witchy, age 3",
                "sum: 13",
                "flag true",
                "literal ${x} stays",
                "witchywitchy",
            ]
        );
    }

    #[test]
    fn guard_example_runs_on_wasm() {
        // Early `return` from a function and from inside a `for` loop.
        let src = include_str!("../examples/guard.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["negative", "zero", "positive", "8", "-1"]);
    }

    #[test]
    fn higher_order_example_runs_on_wasm() {
        // Closure returned from a function (make_adder) + higher-order reduce.
        let src = include_str!("../examples/higher_order.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["15", "81", "15", "120"]);
    }

    #[test]
    fn record_update_example_runs_on_wasm() {
        // `update` referencing the original record, plus a String-field update;
        // the original is unchanged.
        let src = include_str!("../examples/record_update.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(
            run_on_wasm(src),
            vec!["alice 100", "alice 150", "alice smith 150"]
        );
    }

    #[test]
    fn closures_capturing_loop_var_backends_agree() {
        // Closures created in a loop each capture that iteration's value of the
        // loop variable (by value), are stored in a list, and called back. Both
        // backends agree — no shared-loop-variable surprise.
        let src = r#"
fn main(console: Console):
    var fs = []
    for i in [1, 2, 3]:
        fs = push(fs, fn(x: Int): (x + i))
    let f0 = at(fs, 0)
    let f2 = at(fs, 2)
    print(console, int_to_string(f0(10)))
    print(console, int_to_string(f2(10)))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["11", "13"]);
    }

    #[test]
    fn zero_arg_closures_backends_agree() {
        // Zero-argument closures (incl. capturing ones) compile and run.
        let src = r#"
fn call0(f: fn() -> Int) -> Int:
    f()

fn main(console: Console):
    print(console, int_to_string(call0(fn(): 42)))
    let base = 100
    print(console, int_to_string(call0(fn(): (base + 1))))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["42", "101"]);
    }

    #[test]
    fn closure_capturing_closure_backends_agree() {
        // A closure that captures another closure and calls it through a
        // higher-order function. Both backends agree.
        let src = r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main(console: Console):
    let g = fn(x: Int): (x + 1)
    let h = fn(y: Int): (apply(g, y) * 2)
    print(console, int_to_string(apply(h, 5)))
    print(console, int_to_string(apply(h, 20)))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["12", "42"]); // (5+1)*2, (20+1)*2
    }

    #[test]
    fn compound_assignment_backends_agree() {
        // `x op= e` desugars to `x = x op e`; verify all five ops in both
        // backends, in a loop and a sequence.
        let src = r#"
fn main(console: Console):
    var sum = 0
    var i = 0
    while (i < 5):
        sum = (sum + i)
        i = (i + 1)
    print(console, int_to_string(sum))
    var x = 100
    x = (x - 30)
    x = (x * 2)
    x = (x / 7)
    x = (x % 5)
    print(console, int_to_string(x))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["10", "0"]);
    }

    // `replace` with an empty `from` is a notorious edge (the interpreter's
    // Rust `str::replace` inserts the replacement around every character);
    // Int-keyed dicts exercise the by-value key-comparison path. Both must
    // match the compiled backend exactly. Agreement-only, so a future divergence
    // is caught without baking in a hand-computed expectation.
    #[test]
    fn replace_and_int_keyed_dict_backends_agree() {
        let src = r#"
fn main(console: Console):
    print(console, (("[" <> replace("abc", "", "-")) <> "]"))
    print(console, replace("abc", "x", "y"))
    print(console, replace("aaa", "a", "bb"))
    print(console, replace("hello world", "o", "0"))
    var d = dict_new()
    d = insert(d, 1, 100)
    d = insert(d, 2, 200)
    d = insert(d, 1, 111)
    print(console, int_to_string(get_or(d, 1, 0)))
    print(console, int_to_string(get_or(d, 2, 0)))
    print(console, int_to_string(size(d)))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "replace/int-key dict diverged");
    }

    // Integer division/modulo truncate toward zero, and their signs must agree
    // for negative operands across the i64 interpreter and i32 codegen (the
    // results here stay well within i32). Also locks in dict insert-overwrite,
    // removing an absent key, and `get_or`'s default path.
    #[test]
    fn negative_arithmetic_and_dict_mutation_backends_agree() {
        let src = r#"
fn main(console: Console):
    print(console, int_to_string((0 - (7 / 2))))
    print(console, int_to_string(((0 - 7) % 2)))
    print(console, int_to_string((7 / (0 - 2))))
    print(console, int_to_string((7 % (0 - 2))))
    print(console, int_to_string(((0 - 7) / (0 - 2))))
    var d = dict_new()
    d = insert(d, "k", 1)
    d = insert(d, "k", 2)
    print(console, int_to_string(get_or(d, "k", 0)))
    print(console, int_to_string(size(d)))
    d = remove(d, "missing")
    print(console, int_to_string(size(d)))
    print(console, int_to_string(get_or(d, "absent", 99)))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "int/dict edges diverged");
    }

    // Boundary behavior of the string builtins: an empty separator yields the
    // whole string, substrings clamp (and start>end gives ""), an empty needle
    // for index_of returns 0, a missing one returns -1, and empty-string concat
    // is identity. These clamp/empty rules are easy to get subtly different in
    // codegen, so assert the backends agree.
    // Immediately applying a function-valued expression — `make(3)(4)`,
    // `(fn(x){..})(7)`, chains, and an application nested inside another's
    // argument — must behave identically on both backends. The nested-in-arg
    // case in particular exercises codegen's per-level scratch locals (the
    // callee pointer must survive argument evaluation).
    // Function values stored in data structures and applied immediately — the
    // composition unlocked by Expr::Apply. A closure pulled from a list with
    // `at`, one selected by an `if` expression, and one held in a record field
    // (reached via `(b.f)(b.n)`) must all apply identically on both backends.
    #[test]
    fn fn_values_in_data_backends_agree() {
        let src = r#"
type Box:
    f: fn(Int) -> Int
    n: Int

fn main(console: Console):
    let fns = [fn(x: Int): (x + 1), fn(x: Int): (x * 10)]
    print(console, int_to_string((at(fns, 0))(5)))
    print(console, int_to_string((at(fns, 1))(5)))
    let pick = true
    print(console, int_to_string((if pick: fn(x: Int): (x + 100) else: fn(x: Int): x)(7)))
    let b = Box(fn(x: Int): (x * 3), 7)
    print(console, int_to_string(((b).f)((b).n)))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "fn-values-in-data diverged");
        assert_eq!(run_on_wasm(src), vec!["6", "50", "107", "21"]);
    }

    // A guard on a constructor pattern must bind the field first, then test it
    // (`Yep(n) if n > 10`), and fall through to the next arm when the guard
    // fails. Mutual recursion exercises forward references between compiled
    // functions. Both must agree across backends.
    #[test]
    fn adt_guards_and_mutual_recursion_backends_agree() {
        let src = r#"
type Opt:
    Nope
    Yep(Int)

fn describe(o: Opt) -> String:
    match o:
        Yep(n) if (n > 10) -> "big"
        Yep(n) -> "small"
        Nope -> "none"

fn is_even(n: Int) -> Bool:
    if (n == 0):
        true
    else:
        is_odd((n - 1))

fn is_odd(n: Int) -> Bool:
    if (n == 0):
        false
    else:
        is_even((n - 1))

fn main(console: Console):
    print(console, describe(Yep(50)))
    print(console, describe(Yep(3)))
    print(console, describe(Nope))
    print(console, int_to_string(if is_even(10): 1 else: 0))
    print(console, int_to_string(if is_odd(7): 1 else: 0))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "adt guards / mutual recursion diverged");
        assert_eq!(run_on_wasm(src), vec!["big", "small", "none", "1", "1"]);
    }

    // `match` arm guards (`pattern if cond -> body`): a guard that fails must
    // fall through to later arms, and a wildcard catches the rest. The boundary
    // value 100 (not > 100) must fall through to the `_` arm on both backends.
    #[test]
    fn match_guards_backends_agree() {
        let src = r#"
fn classify(n: Int) -> String:
    match n:
        x if (x < 0) -> "negative"
        0 -> "zero"
        x if (x > 100) -> "big"
        _ -> "small"

fn main(console: Console):
    print(console, classify((0 - 5)))
    print(console, classify(0))
    print(console, classify(200))
    print(console, classify(50))
    print(console, classify(100))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "match guards diverged");
        assert_eq!(run_on_wasm(src), vec!["negative", "zero", "big", "small", "small"]);
    }

    // Dict operations factored into helper functions: codegen picks the
    // string-vs-i32 key comparison from the static key type, so a `k: String`
    // parameter must compile to by-value comparison just like an inline String
    // key. Looking up with a freshly built string (`"ap" <> "ple"`) proves the
    // match is structural, not by pointer — and both backends must agree.
    // An integration stress test for first-class functions: a list of closures
    // folded with a higher-order lambda that applies each function-typed
    // element to the accumulator (`f(acc)`). Exercises closures stored in a
    // list, a function-typed fold element, and calling a function-valued lambda
    // parameter — all of which must agree across backends.
    // Nested records: `l.from.x` requires codegen to resolve the record type of
    // the intermediate field (`l.from` is a Point) to index the next one. Record
    // update rebuilds the outer record with one field replaced, leaving the rest
    // (and the original value) untouched. Both backends must agree.
    #[test]
    fn nested_records_and_update_backends_agree() {
        let src = r#"
type Point:
    x: Int
    y: Int

type Line:
    from: Point
    to: Point

fn main(console: Console):
    let l = Line(Point(1, 2), Point(3, 4))
    print(console, int_to_string(((l).from).x))
    print(console, int_to_string(((l).to).y))
    let l2 = Line(from: Point(10, 20), ..l)
    print(console, int_to_string(((l2).from).x))
    print(console, int_to_string(((l2).to).y))
    print(console, int_to_string(((l).from).x))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "nested records / update diverged");
        assert_eq!(run_on_wasm(src), vec!["1", "4", "10", "4", "1"]);
    }

    // A recursive ADT (binary tree) with nested constructor patterns, exercised
    // by two recursive traversals (sum and depth). Recursion through a
    // heap-allocated ADT and destructuring `Node(l, v, r)` must agree across
    // backends.
    #[test]
    fn recursive_tree_adt_backends_agree() {
        let src = r#"
type Tree:
    Leaf
    Node(Tree, Int, Tree)

fn sum_tree(t: Tree) -> Int:
    match t:
        Leaf -> 0
        Node(l, v, r) -> ((sum_tree(l) + v) + sum_tree(r))

fn depth(t: Tree) -> Int:
    match t:
        Leaf -> 0
        Node(l, v, r) ->
            let dl = depth(l)
            let dr = depth(r)
            (1 + if (dl > dr): dl else: dr)

fn main(console: Console):
    let t = Node(Node(Leaf, 1, Node(Leaf, 5, Leaf)), 2, Node(Leaf, 3, Leaf))
    print(console, int_to_string(sum_tree(t)))
    print(console, int_to_string(depth(t)))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "recursive tree ADT diverged");
        assert_eq!(run_on_wasm(src), vec!["11", "3"]);
    }

    // Tuple patterns in `match`: literals and wildcards in each position
    // (quadrant), plus binding tuple elements alongside a literal in another
    // position (describe). Destructuring a matched tuple must agree across
    // backends.
    // List comprehensions desugar to a block that builds the list with a for
    // loop and push: `[elem for x in xs (if cond)?]`. Mapping, filtering, and an
    // empty source all agree across backends.
    // Comprehensions compose with records: the element expression and the `if`
    // filter both access fields of the loop variable (resolved because the
    // source is a List(Record)). Both backends agree.
    #[test]
    fn list_comprehension_over_records_backends_agree() {
        let client = r#"
import list
type Item:
    name: String
    qty: Int
fn main(console: Console):
    let cart = [Item("apple", 3), Item("bread", 1), Item("milk", 2)]
    let multi = [it.name for it in cart if it.qty > 1]
    for n in multi:
        print(console, n)
    let qtys = [it.qty * 10 for it in cart]
    for q in qtys:
        print(console, int_to_string(q))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "comprehension over records diverged");
        assert_eq!(compiled, vec!["apple", "milk", "30", "10", "20"]);
    }

    // Multi-generator comprehensions nest in source order: two `for` clauses
    // form a cartesian product, and an interleaved `if` filters using earlier
    // loop variables. Both backends agree.
    // Integration showcase: Pythagorean triples in one comprehension —
    // three nested generators over inclusive ranges with variable bounds
    // (`b in a..=20`), a filter, and tuple construction, then tuple
    // destructuring in a for-loop. Exercises ranges + multi-generator
    // comprehensions + tuples together; both backends agree.
    #[test]
    fn pythagorean_triples_comprehension_backends_agree() {
        let client = r#"
import list
fn main(console: Console):
    let triples = [(a, b, c) for a in 1..=20 for b in a..=20 for c in b..=20 if a * a + b * b == c * c]
    print(console, int_to_string(length(triples)))
    var total = 0
    for t in triples:
        let (a, b, c) = t
        total = total + c
    print(console, int_to_string(total))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "pythagorean comprehension diverged");
        assert_eq!(compiled, vec!["6", "80"]);
    }

    #[test]
    fn multi_generator_comprehension_backends_agree() {
        let src = r#"
fn main(console: Console):
    let pairs = [x * 10 + y for x in [1, 2] for y in [3, 4]]
    for p in pairs:
        print(console, int_to_string(p))
    let upper = [x * 10 + y for x in [1, 2, 3] for y in [1, 2, 3] if y > x]
    for p in upper:
        print(console, int_to_string(p))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "multi-generator comprehension diverged");
        assert_eq!(
            run_on_wasm(src),
            vec!["13", "14", "23", "24", "12", "13", "23"]
        );
    }

    // break exits the innermost loop; continue skips to the next iteration —
    // in both for-loops (continue advances the index) and while-loops (continue
    // re-checks the condition). Both backends agree.
    // break/continue branching out of a result-typed `match` arm inside a loop
    // must still produce valid WASM (the branch unwinds the match's value).
    #[test]
    fn break_inside_match_in_loop_backends_agree() {
        let src = r#"
fn main(console: Console):
    var total = 0
    for x in [1, 2, 3, 4, 5]:
        match x:
            3 ->
                break
            _ ->
                total = (total + x)
    print(console, int_to_string(total))
    var kept = 0
    for y in [1, 2, 3, 4]:
        match y:
            2 ->
                continue
            _ -> 0
        kept = (kept + y)
    print(console, int_to_string(kept))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "break/continue in match diverged");
        assert_eq!(run_on_wasm(src), vec!["3", "8"]);
    }

    #[test]
    fn break_continue_backends_agree() {
        let src = r#"
fn main(console: Console):
    var sum = 0
    for x in [1, 2, 3, 4, 5, 6, 7, 8]:
        if (x > 5):
            break
        if ((x % 2) == 0):
            continue
        sum = (sum + x)
    print(console, int_to_string(sum))
    var i = 0
    var found = 0
    while (i < 100):
        i = (i + 1)
        if (i < 10):
            continue
        found = i
        break
    print(console, int_to_string(found))
    var count = 0
    for a in [1, 2, 3]:
        for b in [1, 2, 3]:
            if (b == 2):
                break
            count = (count + 1)
    print(console, int_to_string(count))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "break/continue diverged");
        assert_eq!(run_on_wasm(src), vec!["9", "10", "3"]);
    }

    // The `a..b` range operator builds the half-open list [a, b): usable in a
    // for-loop, in a comprehension, and empty when a >= b. Both backends agree.
    // Inclusive range `a..=b` includes the upper bound: [a, b]. Empty when
    // a > b, single when a == b, and composes with comprehensions. Both backends agree.
    #[test]
    fn inclusive_range_backends_agree() {
        let src = r#"
fn main(console: Console):
    for i in 1..=5:
        print(console, int_to_string(i))
    print(console, int_to_string(length(0..=0)))
    print(console, int_to_string(length(5..=2)))
    print(console, int_to_string(length([n for n in 1..=4])))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "inclusive range diverged");
        assert_eq!(run_on_wasm(src), vec!["1", "2", "3", "4", "5", "1", "0", "4"]);
    }

    #[test]
    fn range_operator_backends_agree() {
        let src = r#"
fn main(console: Console):
    for i in 0..5:
        print(console, int_to_string(i))
    let squares = [x * x for x in 1..5]
    for s in squares:
        print(console, int_to_string(s))
    print(console, int_to_string(length(3..3)))
    print(console, int_to_string(length(2..(1 + 4))))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "range operator diverged");
        assert_eq!(
            run_on_wasm(src),
            vec!["0", "1", "2", "3", "4", "1", "4", "9", "16", "0", "3"]
        );
    }

    #[test]
    fn list_comprehension_backends_agree() {
        let src = r#"
fn main(console: Console):
    let squares = [n * n for n in [1, 2, 3, 4]]
    for s in squares:
        print(console, int_to_string(s))
    let evens = [n for n in [1, 2, 3, 4, 5, 6] if n % 2 == 0]
    for e in evens:
        print(console, int_to_string(e))
    print(console, int_to_string(length([x for x in [] if x > 0])))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "list comprehension diverged");
        assert_eq!(run_on_wasm(src), vec!["1", "4", "9", "16", "2", "4", "6", "0"]);
    }

    #[test]
    fn tuple_patterns_backends_agree() {
        let src = r#"
fn quadrant(x: Int, y: Int) -> String:
    match (x, y):
        (0, 0) -> "origin"
        (0, _) -> "y-axis"
        (_, 0) -> "x-axis"
        _ -> "other"

fn describe(pair: (Int, String)) -> String:
    match pair:
        (0, s) -> ("zero:" <> s)
        (n, "stop") -> ("stop@" <> int_to_string(n))
        (n, s) -> ((s <> "=") <> int_to_string(n))

fn main(console: Console):
    print(console, quadrant(0, 0))
    print(console, quadrant(0, 5))
    print(console, quadrant(5, 0))
    print(console, quadrant(2, 3))
    print(console, describe((0, "x")))
    print(console, describe((7, "stop")))
    print(console, describe((4, "k")))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "tuple patterns diverged");
        assert_eq!(
            run_on_wasm(src),
            vec!["origin", "y-axis", "x-axis", "other", "zero:x", "stop@7", "k=4"]
        );
    }

    // The classic loop-capture pitfall: each iteration creates a closure that
    // captures a fresh `let` binding. Capture is by value at creation, so the
    // three closures must remember 0, 1, 2 (giving 10, 11, 12) — not share the
    // final loop value. Both backends must agree.
    #[test]
    fn closure_captures_loop_value_backends_agree() {
        let src = r#"
fn main(console: Console):
    var fns = []
    var i = 0
    while (i < 3):
        let captured = i
        fns = push(fns, fn(x: Int): (x + captured))
        i = (i + 1)
    for f in fns:
        print(console, int_to_string(f(10)))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "loop-captured closures diverged");
        assert_eq!(run_on_wasm(src), vec!["10", "11", "12"]);
    }

    // Generic functions instantiated at several distinct types within one
    // program: pair_up / first_of / second_of are used at (Int,Int),
    // (String,String), and (Int,String). Per-call generalization must give each
    // call site its own instantiation, and both backends must agree.
    #[test]
    fn generics_at_multiple_types_backends_agree() {
        let src = r#"
fn pair_up(x: a, y: b) -> (a, b):
    (x, y)

fn first_of(p: (a, b)) -> a:
    let (f, s) = p
    f

fn second_of(p: (a, b)) -> b:
    let (f, s) = p
    s

fn main(console: Console):
    let pi = pair_up(1, 2)
    let ps = pair_up("a", "b")
    let pm = pair_up(7, "mixed")
    print(console, int_to_string(first_of(pi)))
    print(console, first_of(ps))
    print(console, second_of(ps))
    print(console, int_to_string(first_of(pm)))
    print(console, second_of(pm))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "multi-type generics diverged");
        assert_eq!(run_on_wasm(src), vec!["1", "a", "b", "7", "mixed"]);
    }

    // `update` on a base that is not a bare variable: a field access (`l.from`)
    // and an `if` expression. Codegen used to require a record-typed variable;
    // it now evaluates an arbitrary base once into a scratch slot, matching the
    // interpreter. Nested update in an override (`update p { x: update q ... }`)
    // exercises the level-scoped scratch reuse.
    #[test]
    fn record_update_on_expression_base_backends_agree() {
        let src = r#"
type Point:
    x: Int
    y: Int

type Line:
    from: Point
    to: Point

fn main(console: Console):
    let l = Line(Point(1, 2), Point(3, 4))
    let p2 = Point(x: 100, ..(l).from)
    print(console, int_to_string((p2).x))
    print(console, int_to_string((p2).y))
    let cond = true
    let p3 = Point(y: 99, ..(if cond: (l).from else: (l).to))
    print(console, int_to_string((p3).x))
    print(console, int_to_string((p3).y))
    let l2 = Line(from: Point(x: 7, ..(l).to), ..l)
    print(console, int_to_string(((l2).from).x))
    print(console, int_to_string(((l2).from).y))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "record update on expression base diverged");
        assert_eq!(run_on_wasm(src), vec!["100", "2", "1", "99", "7", "4"]);
    }

    // Iterating records produced by a non-variable expression: a call returning
    // `List(Record)` and a list literal of records. Codegen now resolves the
    // loop variable's record type (so `p.x` works in the body) for any list
    // expression, not just a bare variable — matching the interpreter.
    #[test]
    fn for_over_nonvar_record_list_backends_agree() {
        let src = r#"
type P:
    x: Int
    y: Int

fn mk() -> List(P):
    [P(1, 2), P(3, 4), P(5, 6)]

fn main(console: Console):
    for p in mk():
        print(console, int_to_string(((p).x + (p).y)))
    for q in [P(10, 1), P(20, 2)]:
        print(console, int_to_string((q).x))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "for over non-var record list diverged");
        assert_eq!(run_on_wasm(src), vec!["3", "7", "11", "10", "20"]);
    }

    #[test]
    fn function_pipeline_fold_backends_agree() {
        let client = r#"
import list

fn main(console: Console):
    let inc = fn(x: Int): (x + 1)
    let dbl = fn(x: Int): (x * 2)
    let neg = fn(x: Int): (0 - x)
    let pipeline = [inc, dbl, neg]
    let result = list.fold(pipeline, 5, fn(acc: Int, f: fn(Int) -> Int): f(acc))
    print(console, int_to_string(result))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "function-pipeline fold diverged");
        assert_eq!(compiled, vec!["-12"]);
    }

    // char_count returns Unicode scalars; string_length returns bytes. They
    // agree for ASCII and diverge for multi-byte UTF-8 ("café" is 4 chars, 5
    // bytes) — and both backends must compute each identically.
    #[test]
    fn char_count_vs_string_length_backends_agree() {
        let src = r#"
fn main(console: Console):
    print(console, int_to_string(char_count("hello")))
    print(console, int_to_string(string_length("hello")))
    print(console, int_to_string(char_count("café")))
    print(console, int_to_string(string_length("café")))
    print(console, int_to_string(char_count("")))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "char_count diverged");
        assert_eq!(run_on_wasm(src), vec!["5", "5", "4", "5", "0"]);
    }

    // reverse flips character order using char_count + char-based substring, so
    // it's correct for multi-byte UTF-8 ("café" -> "éfac"), not just ASCII.
    // Char-based take/drop: clamp at the ends and count by Unicode scalar, so
    // they slice "café" correctly (take 2 -> "ca", drop 3 -> "é").
    #[test]
    fn std_string_take_drop_backends_agree() {
        let client = r#"
import string

fn main(console: Console):
    print(console, string.take("hello", 3))
    print(console, (("[" <> string.take("hi", 10)) <> "]"))
    print(console, (("[" <> string.take("hi", 0)) <> "]"))
    print(console, string.drop("hello", 2))
    print(console, (("[" <> string.drop("hi", 5)) <> "]"))
    print(console, string.take("café", 2))
    print(console, string.drop("café", 3))
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "string take/drop diverged");
        assert_eq!(compiled, vec!["hel", "[hi]", "[]", "llo", "[]", "ca", "é"]);
    }

    #[test]
    fn std_string_reverse_backends_agree() {
        let client = r#"
import string

fn main(console: Console):
    print(console, string.reverse("hello"))
    print(console, (("[" <> string.reverse("")) <> "]"))
    print(console, string.reverse("a"))
    print(console, string.reverse("café"))
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "string reverse diverged");
        assert_eq!(compiled, vec!["olleh", "[]", "a", "éfac"]);
    }

    // to_chars splits a string into single-character strings by Unicode scalar
    // (so "café" yields 4 chars including the multi-byte é). Both backends agree.
    // words splits on any whitespace (tabs/newlines/CRs treated as spaces) and
    // drops empty pieces from runs of whitespace or trailing space.
    // split_once splits at the first separator into (before, after); the
    // separator is dropped, later occurrences stay in `after`, and an absent
    // separator gives (s, ""). Both backends agree.
    // replace_first swaps only the first occurrence (unlike the all-replacing
    // `replace` builtin); an absent needle leaves the string unchanged.
    #[test]
    fn std_string_replace_first_backends_agree() {
        let client = r#"
import string

fn main(console: Console):
    print(console, string.replace_first("a.b.c", ".", "/"))
    print(console, string.replace_first("hello", "l", "L"))
    print(console, string.replace_first("xyz", "q", "Q"))
    print(console, string.replace_first("aa", "a", "bb"))
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "replace_first diverged");
        assert_eq!(compiled, vec!["a/b.c", "heLlo", "xyz", "bba"]);
    }

    #[test]
    fn std_string_split_once_backends_agree() {
        let client = r#"
import string

fn main(console: Console):
    let (k, v) = string.split_once("name=witchy", "=")
    print(console, k)
    print(console, v)
    let (a, b) = string.split_once("no-sep-here", "=")
    print(console, a)
    print(console, (("[" <> b) <> "]"))
    let (h, rest) = string.split_once("a=b=c", "=")
    print(console, h)
    print(console, rest)
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "split_once diverged");
        assert_eq!(compiled, vec!["name", "witchy", "no-sep-here", "[]", "a", "b=c"]);
    }

    #[test]
    fn std_string_words_backends_agree() {
        let client = r#"
import string

fn main(console: Console):
    let ws = string.words("the  quick\tbrown\nfox ")
    print(console, int_to_string(length(ws)))
    for w in ws:
        print(console, w)
    print(console, int_to_string(length(string.words("   "))))
    print(console, int_to_string(length(string.words(""))))
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "words diverged");
        assert_eq!(compiled, vec!["4", "the", "quick", "brown", "fox", "0", "0"]);
    }

    #[test]
    fn std_string_to_chars_backends_agree() {
        let client = r#"
import string

fn main(console: Console):
    let cs = string.to_chars("café")
    print(console, int_to_string(length(cs)))
    for c in cs:
        print(console, c)
    print(console, int_to_string(length(string.to_chars(""))))
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "to_chars diverged");
        assert_eq!(compiled, vec!["4", "c", "a", "f", "é", "0"]);
    }

    #[test]
    fn std_string_is_empty_count_backends_agree() {
        // is_empty checks for zero characters; count returns non-overlapping
        // occurrences (0 for an empty needle, and overlapping matches don't
        // double-count: "aaaa"/"aa" is 2). Both backends agree.
        let client = r#"
import string

fn main(console: Console):
    print(console, to_string(string.is_empty("")))
    print(console, to_string(string.is_empty("x")))
    print(console, int_to_string(string.count("banana", "a")))
    print(console, int_to_string(string.count("banana", "an")))
    print(console, int_to_string(string.count("aaaa", "aa")))
    print(console, int_to_string(string.count("abc", "x")))
    print(console, int_to_string(string.count("abc", "")))
    print(console, int_to_string(string.count("aéaéa", "éa")))
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "string is_empty/count diverged");
        // The last counts a multi-byte needle: "éa" occurs twice in "aéaéa" —
        // a byte-based advance would miscount it (and matters only off ASCII).
        assert_eq!(compiled, vec!["true", "false", "3", "2", "2", "0", "0", "2"]);
    }

    #[test]
    fn std_string_char_at_backends_agree() {
        // char_at returns the single character at an index, or "" out of range.
        let client = r#"
import string

fn main(console: Console):
    print(console, string.char_at("witchy", 0))
    print(console, string.char_at("witchy", 5))
    print(console, (("[" <> string.char_at("witchy", 10)) <> "]"))
    print(console, (("[" <> string.char_at("", 0)) <> "]"))
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "char_at diverged");
        assert_eq!(compiled, vec!["w", "y", "[]", "[]"]);
    }

    #[test]
    fn dict_string_key_through_helpers_backends_agree() {
        let src = r#"
fn put(d: Dict(String, Int), k: String, v: Int) -> Dict(String, Int):
    insert(d, k, v)

fn lookup(d: Dict(String, Int), k: String) -> Int:
    get_or(d, k, (0 - 1))

fn main(console: Console):
    var d = dict_new()
    d = put(d, "apple", 1)
    d = put(d, "banana", 2)
    print(console, int_to_string(lookup(d, ("ap" <> "ple"))))
    print(console, int_to_string(lookup(d, "banana")))
    print(console, int_to_string(lookup(d, "cherry")))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "dict string-key via helpers diverged");
        assert_eq!(run_on_wasm(src), vec!["1", "2", "-1"]);
    }

    // Regression: a local variable that shares its name with a same-module
    // function must stay a local, not be rewritten into a first-class reference
    // to that function by the linker. (The function-as-value feature qualifies
    // bare function-name Vars; it must skip names shadowed by a local.)
    #[test]
    fn local_shadowing_function_name_backends_agree() {
        let client = r#"
fn size(n: Int) -> Int:
    (n * 100)

fn main(console: Console):
    var size = 3
    size = (size + 4)
    print(console, int_to_string(size))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "local shadowing a function name diverged");
        assert_eq!(compiled, vec!["7"]);
    }

    // The linker treats the bundled std as a built-in search path: a program can
    // `import list` without the caller listing the module's source. This unblocks
    // composable std modules (one std module importing another). Verified on both
    // backends with only `main` provided.
    #[test]
    fn linker_auto_resolves_std_imports() {
        let client = r#"
import list

fn main(console: Console):
    print(console, int_to_string(list.sum(list.range(5))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "auto-resolved std import diverged");
        assert_eq!(compiled, vec!["10"]);
    }

    // The composable, total lookups: list.head/last/get/find return Option
    // (None instead of an out-of-bounds trap). `list` imports `option`, and the
    // caller provides only `main` — the linker auto-resolves both std modules.
    // More total Option-returning list functions: min/max (None for the empty
    // list) and position (the Option counterpart to index_of's -1 sentinel).
    // result -> option conversions (result imports option): `ok` keeps the Ok
    // value as Some and drops an Err to None; `err` does the reverse. Caller
    // provides only `main`; the linker resolves result and option.
    // option -> result conversions (option imports result, completing the
    // Option<->Result pair; the linker flattens the cyclic import). ok_or maps
    // Some to Ok and None to Err(err); ok_or_else computes the error lazily.
    #[test]
    fn std_option_to_result_backends_agree() {
        let client = r#"
import option
import result

fn main(console: Console):
    print(console, int_to_string(result.unwrap_or(option.ok_or(Some(5), "none"), 0)))
    print(console, to_string(result.is_err(option.ok_or(None, "none"))))
    print(console, int_to_string(result.unwrap_or(option.ok_or_else(Some(9), fn(): "none"), 0)))
    print(console, to_string(result.is_err(option.ok_or_else(None, fn(): "none"))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "option->result diverged");
        assert_eq!(compiled, vec!["5", "true", "9", "true"]);
    }

    // result.flatten collapses Result(Result(a, e), e) one level (Ok(Ok(v)) ->
    // Ok(v); Ok(Err) and Err -> Err), mirroring option.flatten. Both backends agree.
    #[test]
    fn std_result_flatten_backends_agree() {
        let client = r#"
import result

fn nested(n: Int) -> Result(Result(Int, String), String):
    if (n > 0):
        Ok(Ok(n))
    else:
        Ok(Err("inner"))

fn main(console: Console):
    print(console, int_to_string(result.unwrap_or(result.flatten(nested(5)), 0)))
    print(console, to_string(result.is_err(result.flatten(nested(0)))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "result flatten diverged");
        assert_eq!(compiled, vec!["5", "true"]);
    }

    #[test]
    fn std_result_to_option_backends_agree() {
        let client = r#"
import result
import option

fn check(n: Int) -> Result(Int, String):
    if (n > 0):
        Ok(n)
    else:
        Err("bad")

fn main(console: Console):
    print(console, int_to_string(option.unwrap_or(result.ok(check(5)), 0)))
    print(console, to_string(option.is_none(result.ok(check((0 - 1))))))
    print(console, to_string(option.is_none(result.err(check(5)))))
    print(console, int_to_string(string_length(option.unwrap_or(result.err(check((0 - 1))), ""))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "result->option diverged");
        assert_eq!(compiled, vec!["5", "true", "true", "3"]);
    }

    // sort (ascending Int convenience over sort_by) and unique (drop duplicates,
    // keeping the first occurrence in order). Both backends agree.
    #[test]
    fn std_list_sort_unique_backends_agree() {
        let client = r#"
import list

fn main(console: Console):
    let s = list.sort([3, 1, 4, 1, 5, 9, 2, 6])
    for x in s:
        print(console, int_to_string(x))
    let u = list.unique([1, 2, 2, 3, 1, 4, 3])
    print(console, int_to_string(length(u)))
    for x in u:
        print(console, int_to_string(x))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "sort/unique diverged");
        assert_eq!(
            compiled,
            vec!["1", "1", "2", "3", "4", "5", "6", "9", "4", "1", "2", "3", "4"]
        );
    }

    // max_by/min_by generalize min/max to any type via a comparator, returning
    // Option. The second comparator (`(0-a) < (0-b)`, i.e. larger magnitude is
    // "less") shows the result tracks the supplied ordering, not the natural one.
    // A variable bound to a record-typed constructor field in a match pattern
    // (`Circle(c)`) now resolves field access in the arm body (`c.x`). Codegen
    // previously rejected this; it's fixed for concrete (non-generic) field
    // types. Both backends agree.
    // Matching the Some of a function-returned Option(Record) binds the payload
    // to its record type, so `a.balance` resolves. Codegen learns the payload
    // record from the function's declared `-> Option(Account)` return.
    // Let-bound intermediates inherit derived types: `let o = lookup()` carries
    // the Option(Account) payload (so a later `match o { Some(a) -> a.balance }`
    // resolves), and `let xs = mk()` carries the List(P) element type (so
    // `for p in xs { p.x }` resolves). Both backends agree.
    #[test]
    fn let_bound_derived_types_backends_agree() {
        let client = r#"
import option

type Account:
    id: Int
    balance: Int

type P:
    x: Int
    y: Int

fn lookup(n: Int) -> Option(Account):
    if (n > 0):
        Some(Account(n, (n * 100)))
    else:
        None

fn mk() -> List(P):
    [P(1, 2), P(3, 4)]

fn main(console: Console):
    let o = lookup(7)
    match o:
        Some(a) -> print(console, int_to_string((a).balance))
        None -> print(console, "none")
    let xs = mk()
    for p in xs:
        print(console, int_to_string((p).x))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "let-bound derived types diverged");
        assert_eq!(compiled, vec!["700", "1", "3"]);
    }

    // The generic stdlib case: `list.find` etc. have shape `fn(List(a),..) ->
    // Option(a)`, so matching their result binds the payload to the list's
    // element record type. `acc.field` now resolves through a generic lookup.
    // Generic `fn(List(a),..) -> List(a)` results (filter/reverse/...) carry the
    // argument's element record type, so iterating them resolves field access:
    // `for p in list.filter(records, pred) { p.field }`.
    // map's result element type is the mapper's return type, so iterating a
    // `list.map(records, fn(r){ OtherRecord(..) })` resolves field access on the
    // mapped records (a different record type than the input).
    // End-to-end: records flow through the whole stdlib pipeline with correct
    // field resolution — fold over records, max_by/find returning Option(record)
    // (match payload reads fields), filter then iterate (loop var reads fields),
    // a helper function over a record, and first-class lambdas throughout.
    // The `?` operator unwrapping a Result(Record): `let acc = lookup(n)?` binds
    // acc to the payload record so `acc.balance` resolves, and an Err short-
    // circuits the enclosing Result-returning function. Both backends agree.
    #[test]
    fn try_operator_record_payload_backends_agree() {
        let client = r#"
import result

type Account:
    id: Int
    balance: Int

fn lookup(n: Int) -> Result(Account, String):
    if (n > 0):
        Ok(Account(n, (n * 100)))
    else:
        Err("bad")

fn process(n: Int) -> Result(Int, String):
    let acc = (lookup(n))?
    Ok(((acc).balance + 1))

fn main(console: Console):
    match process(5):
        Ok(v) -> print(console, int_to_string(v))
        Err(e) -> print(console, e)
    match process((0 - 1)):
        Ok(v) -> print(console, int_to_string(v))
        Err(e) -> print(console, e)
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "? with Result(Record) diverged");
        assert_eq!(compiled, vec!["501", "bad"]);
    }

    // Integration showcase: a recursive JSON-value renderer. Exercises a
    // recursive ADT (JArr holds List(Json)), every match arm form, recursion,
    // list.map with a *named function* argument (function-as-value), and
    // string.join — all composing. Both backends agree.
    #[test]
    fn json_renderer_integration_backends_agree() {
        let client = r#"
import list
import string

type Json:
    JNull
    JBool(Bool)
    JNum(Int)
    JStr(String)
    JArr(List(Json))

fn render(j: Json) -> String:
    match j:
        JNull -> "null"
        JBool(b) -> if b: "true" else: "false"
        JNum(n) -> int_to_string(n)
        JStr(s) -> (("\"" <> s) <> "\"")
        JArr(items) -> (("[" <> string.join(list.map(items, render), ",")) <> "]")

fn main(console: Console):
    let doc = JArr([JNum(1), JStr("hi"), JBool(true), JNull, JArr([JNum(2), JNum(3)])])
    print(console, render(doc))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "json renderer diverged");
        assert_eq!(compiled, vec!["[1,\"hi\",true,null,[2,3]]"]);
    }

    #[test]
    fn order_processing_integration_backends_agree() {
        let client = r#"
import list
import option

type Item:
    name: String
    price: Int
    qty: Int

fn line_total(it: Item) -> Int:
    ((it).price * (it).qty)

fn main(console: Console):
    let cart = [Item("apple", 50, 3), Item("bread", 200, 1), Item("milk", 150, 2)]
    let total = list.fold(cart, 0, fn(acc: Int, it: Item): (acc + line_total(it)))
    print(console, int_to_string(total))
    match list.max_by(cart, fn(a: Item, b: Item): (line_total(a) < line_total(b))):
        Some(it) -> print(console, (it).name)
        None -> print(console, "none")
    let multi = list.filter(cart, fn(it: Item): ((it).qty > 1))
    for it in multi:
        print(console, (it).name)
    match list.find(cart, fn(it: Item): ((it).name == "bread")):
        Some(it) -> print(console, int_to_string((it).price))
        None -> print(console, "0")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "order processing diverged");
        assert_eq!(compiled, vec!["650", "milk", "apple", "milk", "200"]);
    }

    #[test]
    fn iterate_map_result_records_backends_agree() {
        let client = r#"
import list

type Raw:
    a: Int
    b: Int

type Point:
    x: Int
    y: Int

fn main(console: Console):
    let raws = [Raw(1, 2), Raw(3, 4)]
    let pts = list.map(raws, fn(r: Raw): Point(((r).a + (r).b), ((r).a * (r).b)))
    for p in pts:
        print(console, int_to_string((p).x))
    for p in list.map(raws, fn(r: Raw): Point((r).b, (r).a)):
        print(console, int_to_string((p).y))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "iterate map result diverged");
        assert_eq!(compiled, vec!["3", "7", "1", "3"]);
    }

    #[test]
    fn iterate_generic_list_result_records_backends_agree() {
        let client = r#"
import list

type P:
    x: Int
    y: Int

fn main(console: Console):
    let ps = [P(1, 10), P(2, 20), P(3, 30)]
    let evens = list.filter(ps, fn(p: P): (((p).x % 2) == 0))
    for p in evens:
        print(console, int_to_string((p).y))
    for p in list.reverse(ps):
        print(console, int_to_string((p).x))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "iterate generic list result diverged");
        assert_eq!(compiled, vec!["20", "3", "2", "1"]);
    }

    #[test]
    fn match_generic_list_lookup_payload_backends_agree() {
        let client = r#"
import list
import option

type Account:
    id: Int
    balance: Int

fn main(console: Console):
    let accounts = [Account(1, 100), Account(2, 200), Account(3, 300)]
    match list.find(accounts, fn(a: Account): ((a).balance > 150)):
        Some(acc) -> print(console, int_to_string((acc).balance))
        None -> print(console, "none")
    match list.head(accounts):
        Some(acc) -> print(console, int_to_string((acc).id))
        None -> print(console, "none")
    match list.find(accounts, fn(a: Account): ((a).balance > 999)):
        Some(acc) -> print(console, int_to_string((acc).id))
        None -> print(console, "none")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "generic list lookup payload diverged");
        assert_eq!(compiled, vec!["200", "1", "none"]);
    }

    #[test]
    fn match_option_record_payload_backends_agree() {
        let client = r#"
import option

type Account:
    id: Int
    balance: Int

fn lookup(n: Int) -> Option(Account):
    if (n > 0):
        Some(Account(n, (n * 100)))
    else:
        None

fn main(console: Console):
    match lookup(5):
        Some(a) -> print(console, int_to_string((a).balance))
        None -> print(console, "none")
    match lookup((0 - 1)):
        Some(a) -> print(console, int_to_string((a).balance))
        None -> print(console, "none")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "Option(Record) match diverged");
        assert_eq!(compiled, vec!["500", "none"]);
    }

    // Nested constructor patterns destructure through a record: `Circle(Point(x,
    // y))` binds x and y from the inner Point in one pattern. Both backends agree.
    #[test]
    fn nested_constructor_pattern_backends_agree() {
        let src = r#"
type Point:
    x: Int
    y: Int

type Shape:
    Circle(Point)
    Origin

fn f(s: Shape) -> Int:
    match s:
        Circle(Point(x, y)) -> (x + y)
        Origin -> 0

fn main(console: Console):
    print(console, int_to_string(f(Circle(Point(3, 4)))))
    print(console, int_to_string(f(Circle(Point(10, 1)))))
    print(console, int_to_string(f(Origin)))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "nested constructor pattern diverged");
        assert_eq!(run_on_wasm(src), vec!["7", "11", "0"]);
    }

    #[test]
    fn match_binds_record_field_backends_agree() {
        let src = r#"
type Point:
    x: Int
    y: Int

type Shape:
    Circle(Point)
    Rect(Int, Int)

fn describe(s: Shape) -> Int:
    match s:
        Circle(c) -> ((c).x + (c).y)
        Rect(w, h) -> (w * h)

fn main(console: Console):
    print(console, int_to_string(describe(Circle(Point(3, 4)))))
    print(console, int_to_string(describe(Rect(5, 6))))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "match record-field bind diverged");
        assert_eq!(run_on_wasm(src), vec!["7", "30"]);
    }

    // find_map searches and transforms in one pass: the first non-None result
    // of f, or None. Here it returns half of the first even number.
    // reduce folds with the first element as the seed (Option-returning, None
    // for empty) — here used as max and sum without an explicit initial value.
    #[test]
    fn std_list_reduce_backends_agree() {
        let client = r#"
import list
import option

fn main(console: Console):
    let mx = list.reduce([3, 1, 4, 1, 5], fn(a: Int, b: Int): if (a > b): a else: b)
    print(console, int_to_string(option.unwrap_or(mx, 0)))
    print(console, to_string(option.is_none(list.reduce([], fn(a: Int, b: Int): (a + b)))))
    let sum = list.reduce([10, 20, 30], fn(a: Int, b: Int): (a + b))
    print(console, int_to_string(option.unwrap_or(sum, 0)))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "reduce diverged");
        assert_eq!(compiled, vec!["5", "true", "60"]);
    }

    #[test]
    fn std_list_find_map_backends_agree() {
        let client = r#"
import list
import option

fn main(console: Console):
    let r = list.find_map([3, 5, 8, 10], fn(x: Int): if ((x % 2) == 0): Some((x / 2)) else: None)
    print(console, int_to_string(option.unwrap_or(r, (0 - 1))))
    let none = list.find_map([1, 3, 5], fn(x: Int): if (x > 100): Some(x) else: None)
    print(console, to_string(option.is_none(none)))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "find_map diverged");
        assert_eq!(compiled, vec!["4", "true"]);
    }

    #[test]
    fn std_list_min_max_by_backends_agree() {
        let client = r#"
import list
import option

fn main(console: Console):
    let xs = [3, 1, 4, 1, 5, 9, 2]
    print(console, int_to_string(option.unwrap_or(list.max_by(xs, fn(a: Int, b: Int): (a < b)), 0)))
    print(console, int_to_string(option.unwrap_or(list.min_by(xs, fn(a: Int, b: Int): (a < b)), 0)))
    print(console, int_to_string(option.unwrap_or(list.max_by(xs, fn(a: Int, b: Int): ((0 - a) < (0 - b))), 0)))
    print(console, to_string(option.is_none(list.max_by([], fn(a: Int, b: Int): (a < b)))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "min_by/max_by diverged");
        assert_eq!(compiled, vec!["9", "1", "1", "true"]);
    }

    #[test]
    fn std_list_min_max_position_backends_agree() {
        let client = r#"
import list
import option

fn main(console: Console):
    print(console, int_to_string(option.unwrap_or(list.min([3, 1, 4, 1, 5]), 0)))
    print(console, int_to_string(option.unwrap_or(list.max([3, 1, 4, 1, 5]), 0)))
    print(console, to_string(option.is_none(list.min([]))))
    print(console, int_to_string(option.unwrap_or(list.position([10, 20, 30], 20), (0 - 1))))
    print(console, to_string(option.is_none(list.position([10, 20], 99))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "list min/max/position diverged");
        assert_eq!(compiled, vec!["1", "5", "true", "1", "true"]);
    }

    #[test]
    fn std_list_option_lookups_backends_agree() {
        let client = r#"
import list
import option

fn main(console: Console):
    print(console, int_to_string(option.unwrap_or(list.head([10, 20]), 0)))
    print(console, int_to_string(option.unwrap_or(list.head([]), (0 - 1))))
    print(console, int_to_string(option.unwrap_or(list.last([10, 20]), 0)))
    print(console, int_to_string(option.unwrap_or(list.get([10, 20, 30], 1), 0)))
    print(console, int_to_string(option.unwrap_or(list.get([10], 5), (0 - 1))))
    print(console, int_to_string(option.unwrap_or(list.find([1, 3, 4], fn(n: Int): ((n % 2) == 0)), (0 - 1))))
    print(console, to_string(option.is_none(list.find([1, 3, 5], fn(n: Int): ((n % 2) == 0)))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "list option lookups diverged");
        assert_eq!(compiled, vec!["10", "-1", "20", "20", "-1", "4", "true"]);
    }

    #[test]
    fn std_list_head_last_find_or_backends_agree() {
        // Total accessors: head_or/last_or return a default for the empty list
        // (never indexing out of bounds), and find_or returns the first match or
        // a default. Both backends agree.
        let client = r#"
import list

fn main(console: Console):
    print(console, int_to_string(list.head_or([10, 20, 30], 0)))
    print(console, int_to_string(list.head_or([], (0 - 1))))
    print(console, int_to_string(list.last_or([10, 20, 30], 0)))
    print(console, int_to_string(list.last_or([], (0 - 1))))
    print(console, int_to_string(list.find_or([1, 3, 4, 7], fn(n: Int): ((n % 2) == 0), (0 - 1))))
    print(console, int_to_string(list.find_or([1, 3, 5], fn(n: Int): ((n % 2) == 0), (0 - 1))))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "head_or/last_or/find_or diverged");
        assert_eq!(compiled, vec!["10", "-1", "30", "-1", "4", "-1"]);
    }

    // windows: sliding sublists of length n (step 1), empty when n exceeds the
    // list or n < 1. Complements chunks. Iterating List(List(Int)) too.
    #[test]
    fn std_list_windows_backends_agree() {
        let client = r#"
import list

fn main(console: Console):
    let ws = list.windows([1, 2, 3, 4], 2)
    print(console, int_to_string(length(ws)))
    for w in ws:
        print(console, int_to_string(list.sum(w)))
    print(console, int_to_string(length(list.windows([1, 2], 5))))
    print(console, int_to_string(length(list.windows([1, 2, 3], 0))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "windows diverged");
        assert_eq!(compiled, vec!["3", "3", "5", "7", "0", "0"]);
    }

    // split_at splits a list into (first n, the rest); n is clamped at both
    // ends. The list analogue of string.split_once. Both backends agree.
    #[test]
    fn std_list_split_at_backends_agree() {
        let client = r#"
import list

fn main(console: Console):
    let (a, b) = list.split_at([1, 2, 3, 4, 5], 2)
    print(console, int_to_string(list.sum(a)))
    print(console, int_to_string(list.sum(b)))
    let (c, d) = list.split_at([1, 2], 5)
    print(console, int_to_string(list.sum(c)))
    print(console, int_to_string(length(d)))
    let (e, f) = list.split_at([1, 2, 3], 0)
    print(console, int_to_string(length(e)))
    print(console, int_to_string(list.sum(f)))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "split_at diverged");
        assert_eq!(compiled, vec!["3", "12", "3", "0", "0", "6"]);
    }

    #[test]
    fn std_list_chunks_tail_init_backends_agree() {
        // chunks groups into fixed-size sublists (last may be short), tail drops
        // the first element, init drops the last — all total (empty stays empty).
        // Iterating List(List(Int)) also exercises nested lists across backends.
        let client = r#"
import list

fn main(console: Console):
    let cs = list.chunks([1, 2, 3, 4, 5], 2)
    print(console, int_to_string(length(cs)))
    for c in cs:
        print(console, int_to_string(list.sum(c)))
    print(console, int_to_string(list.sum(list.tail([1, 2, 3]))))
    print(console, int_to_string(list.sum(list.init([1, 2, 3]))))
    print(console, int_to_string(length(list.tail([]))))
    print(console, int_to_string(length(list.init([]))))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "chunks/tail/init diverged");
        assert_eq!(compiled, vec!["3", "3", "7", "5", "5", "3", "0", "0"]);
    }

    // sum_by totals a projection of each element (0 for empty) — including a
    // record field via a record-typed lambda parameter.
    #[test]
    fn std_list_sum_by_backends_agree() {
        let client = r#"
import list

type Item:
    price: Int
    qty: Int

fn main(console: Console):
    let cart = [Item(50, 3), Item(200, 1), Item(150, 2)]
    print(console, int_to_string(list.sum_by(cart, fn(it: Item): ((it).price * (it).qty))))
    print(console, int_to_string(list.sum_by([1, 2, 3, 4], fn(n: Int): (n * n))))
    print(console, int_to_string(list.sum_by([], fn(n: Int): n)))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "sum_by diverged");
        assert_eq!(compiled, vec!["650", "30", "0"]);
    }

    #[test]
    fn std_list_product_slice_scan_backends_agree() {
        // product (1 for empty), slice (clamped half-open range), and scan
        // (running fold collecting intermediates) all agree across backends.
        let client = r#"
import list

fn main(console: Console):
    print(console, int_to_string(list.product([1, 2, 3, 4])))
    print(console, int_to_string(list.product([])))
    let s = list.slice([10, 20, 30, 40, 50], 1, 4)
    for x in s:
        print(console, int_to_string(x))
    let running = list.scan([1, 2, 3], 0, fn(acc: Int, n: Int): (acc + n))
    for x in running:
        print(console, int_to_string(x))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "product/slice/scan diverged");
        assert_eq!(compiled, vec!["24", "1", "20", "30", "40", "0", "1", "3", "6"]);
    }

    #[test]
    fn std_func_combinators_backends_agree() {
        // The whole `func` module links + compiles, and its combinators — built
        // on first-class functions — agree across backends: compose threads
        // named functions, flip swaps a subtraction's operands, constant
        // ignores its argument, identity is a no-op.
        let client = r#"
import func

fn double(x: Int) -> Int:
    (x * 2)

fn inc(x: Int) -> Int:
    (x + 1)

fn sub(a: Int, b: Int) -> Int:
    (a - b)

fn main(console: Console):
    let h = func.compose(double, inc)
    print(console, int_to_string(h(10)))
    print(console, int_to_string((func.flip(sub))(3, 10)))
    print(console, int_to_string((func.constant(42))(999)))
    print(console, int_to_string(func.identity(7)))
"#;
        let sources = [("func", crate::bundled_module("func").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "func combinators diverged");
        assert_eq!(compiled, vec!["22", "7", "42", "7"]);
    }

    // A closure that *calls* a captured function-valued variable (`f(g(x))`,
    // where f and g are captured) must thread f and g through the closure
    // environment and invoke them indirectly — not emit a direct `call $g`.
    // This is the classic `compose`; it must agree across backends.
    #[test]
    fn compose_captured_functions_backends_agree() {
        let src = r#"
fn compose(f: fn(Int) -> Int, g: fn(Int) -> Int) -> fn(Int) -> Int:
    fn(x: Int): f(g(x))

fn double(x: Int) -> Int:
    (x * 2)

fn inc(x: Int) -> Int:
    (x + 1)

fn main(console: Console):
    let h = compose(double, inc)
    print(console, int_to_string(h(10)))
    print(console, int_to_string((compose(inc, double))(10)))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "compose diverged");
        assert_eq!(run_on_wasm(src), vec!["22", "21"]);
    }

    #[test]
    fn function_by_name_as_value_backends_agree() {
        // A bare top-level function name is a first-class value: bind it, call
        // it, and apply it repeatedly. Both backends materialize it as a
        // callable closure.
        let src = r#"
fn double(x: Int) -> Int:
    (x * 2)

fn inc(x: Int) -> Int:
    (x + 1)

fn main(console: Console):
    let f = double
    print(console, int_to_string(f(5)))
    let g = inc
    print(console, int_to_string(g(g(g(0)))))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "function-as-value diverged");
        assert_eq!(run_on_wasm(src), vec!["10", "3"]);
    }

    #[test]
    fn named_function_passed_to_map_backends_agree() {
        // Point-free style: pass a named function (not a lambda) straight to a
        // higher-order std function. Exercises the linker qualifying a bare
        // function-name reference and codegen forwarding through a closure.
        let client = r#"
import list

fn triple(x: Int) -> Int:
    (x * 3)

fn main(console: Console):
    let ys = list.map([1, 2, 3], triple)
    for y in ys:
        print(console, int_to_string(y))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "named-function-to-map diverged");
        assert_eq!(compiled, vec!["3", "6", "9"]);
    }

    #[test]
    fn immediate_application_backends_agree() {
        let src = r#"
fn twice(f: fn(Int) -> Int, x: Int) -> Int:
    f(f(x))

fn main(console: Console):
    let make_adder = fn(x: Int): fn(y: Int): (x + y)
    let make_mul = fn(a: Int): fn(b: Int): fn(c: Int): ((a * b) * c)
    print(console, int_to_string((make_adder(10))(5)))
    print(console, int_to_string(((make_mul(2))(3))(4)))
    print(console, int_to_string((fn(n: Int): (n * n))(7)))
    print(console, int_to_string(twice(make_adder(1), 10)))
    print(console, int_to_string((make_adder(10))((make_adder(2))(3))))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "immediate application diverged");
        assert_eq!(run_on_wasm(src), vec!["15", "24", "49", "12", "15"]);
    }

    #[test]
    fn closures_and_string_ordering_backends_agree() {
        let src = r#"
fn main(console: Console):
    let base = 10
    let add = fn(n: Int): (n + base)
    var total = 0
    var i = 0
    while (i < 5):
        total = (total + add(i))
        i = (i + 1)
    print(console, int_to_string(total))
    let make_adder = fn(x: Int): fn(y: Int): (x + y)
    let add3 = make_adder(3)
    print(console, int_to_string(add3(4)))
    print(console, int_to_string((make_adder(100))(1)))
    if ("abc" < "abcd"):
        print(console, "lt1")
    else:
        print(console, "ge1")
    if ("Z" < "a"):
        print(console, "lt2")
    else:
        print(console, "ge2")
    if ("" < "a"):
        print(console, "lt3")
    else:
        print(console, "ge3")
    if ("apple" < "apply"):
        print(console, "lt4")
    else:
        print(console, "ge4")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "closures/ordering diverged");
    }

    #[test]
    fn string_edge_cases_backends_agree() {
        let src = r#"
fn main(console: Console):
    print(console, int_to_string(length(split("abc", ""))))
    print(console, int_to_string(length(split("abc", "x"))))
    print(console, int_to_string(length(split("a,b,c", ","))))
    print(console, (("[" <> substring("", 0, 5)) <> "]"))
    print(console, (("[" <> substring("hello", 3, 1)) <> "]"))
    print(console, substring("hello", 2, 100))
    print(console, int_to_string(index_of("hello", "")))
    print(console, int_to_string(index_of("hello", "z")))
    print(console, (("[" <> (("" <> "x") <> "")) <> "]"))
    print(console, int_to_string(string_length("")))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "string edge cases diverged");
    }

    #[test]
    fn trim_backends_agree() {
        // trim now compiles: leading/trailing ASCII whitespace (spaces, tabs,
        // newlines, CRs) is stripped; an all-whitespace string trims to "".
        let src = r#"
fn main(console: Console):
    print(console, trim("  hello  "))
    print(console, trim("\t\nfoo\r\n"))
    print(console, trim("nospaces"))
    print(console, trim("   "))
    print(console, int_to_string(string_length(trim("  a b  "))))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["hello", "foo", "nospaces", "", "3"]);
    }

    // std/json get_path: follow a dotted key path into a decoded value (and a
    // missing path -> None). Pure, so both backends agree.
    #[test]
    fn std_json_get_path_backends_agree() {
        let client = r#"
import json
import option
fn str_at(j: Json, path: String) -> String:
    match json.get_path(j, path):
        Some(v) -> option.unwrap_or(json.as_string(v), "?")
        None -> "none"
fn int_at(j: Json, path: String) -> Int:
    match json.get_path(j, path):
        Some(v) -> option.unwrap_or(json.as_int(v), 0)
        None -> 0
fn main(console: Console):
    match json.decode("{\"user\":{\"name\":\"witchy\",\"age\":1},\"tags\":[\"a\"]}"):
        Ok(j) ->
            print(console, str_at(j, "user.name"))
            print(console, int_to_string(int_at(j, "user.age")))
            print(console, str_at(j, "user.missing"))
        Err(e) -> print(console, e)
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std json get_path diverged");
        assert_eq!(compiled, vec!["witchy", "1", "none"]);
    }

    // Dead-code elimination: a program importing `list` but using only `map`
    // and `sum` must not compile the rest of the list API (or `option`, which
    // `list` imports) into the WASM — only functions reachable from `main`.
    #[test]
    fn dce_drops_unused_stdlib_functions() {
        let client = r#"
import list

fn main(console: Console):
    let xs = list.map([1, 2, 3], fn(x: Int): (x * 2))
    print(console, int_to_string(list.sum(xs)))
"#;
        let mods = vec![("main".to_string(), parser::parse_module(client).expect("parse"))];
        let linked = crate::linker::link(mods, "main").expect("link");
        let wat = codegen::compile_module(&linked).expect("compile");
        // Reachable functions are present.
        assert!(wat.contains("$list.map"), "map should be compiled");
        assert!(wat.contains("$list.sum"), "sum should be compiled");
        // Unused ones are gone.
        assert!(!wat.contains("$list.partition"), "partition should be eliminated");
        assert!(!wat.contains("$list.windows"), "windows should be eliminated");
        assert!(!wat.contains("$list.sort_by"), "sort_by should be eliminated");
        assert!(!wat.contains("$option."), "unused option fns should be eliminated");
        // And it still runs correctly.
        assert_eq!(run_linked_on_wasm(&[("main", client)], "main"), vec!["12"]);
    }

    // std/url: parse assorted URL strings (default ports, explicit port, path,
    // and a malformed one). Pure, so both backends agree.
    #[test]
    fn std_url_parse_backends_agree() {
        let client = r#"
import url
fn describe(s: String) -> String:
    match url.parse(s):
        Some(u) -> url.scheme(u) <> " " <> url.host(u) <> " " <> int_to_string(url.port(u)) <> " " <> url.path(u)
        None -> "invalid"
fn main(console: Console):
    print(console, describe("http://example.com"))
    print(console, describe("http://example.com:8080/foo"))
    print(console, describe("https://x.com/a/b"))
    print(console, describe("notaurl"))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std url parse diverged");
        assert_eq!(
            compiled,
            vec![
                "http example.com 80 /",
                "http example.com 8080 /foo",
                "https x.com 443 /a/b",
                "invalid"
            ]
        );
    }

    // std/http get_url: parse a URL string and GET it (loopback). Interpreter-only.
    #[test]
    fn std_http_get_url_against_loopback() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut req = Vec::new();
                let mut tmp = [0u8; 256];
                while let Ok(n) = stream.read(&mut tmp) {
                    if n == 0 {
                        break;
                    }
                    req.extend_from_slice(&tmp[..n]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let resp = "HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nhello-url";
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        let program = format!(
            r#"
import http
fn main(console: Console, net: Net):
    match http.get_url(net, "http://127.0.0.1:{port}/path"):
        Ok(r) -> print(console, http.body(r))
        Err(e) -> print(console, e)
"#
        );
        let mods = vec![("main".to_string(), parser::parse_module(&program).expect("parse"))];
        let linked = crate::linker::link(mods, "main").expect("link");
        let out = interpreter::run_module(
            linked,
            std::path::Path::new("."),
            vec![format!("127.0.0.1:{port}")],
        )
        .expect("run");
        server.join().ok();
        assert_eq!(out, vec!["hello-url"]);
    }

    // std/string trimming: trim/trim_start/trim_end over assorted whitespace.
    // Pure, so both backends agree.
    #[test]
    fn std_string_trim_backends_agree() {
        let client = r#"
import string
fn main(console: Console):
    print(console, "[" <> string.trim("  hello  ") <> "]")
    print(console, "[" <> string.trim_start("  hi") <> "]")
    print(console, "[" <> string.trim_end("bye  ") <> "]")
    print(console, "[" <> string.trim("\t\n x \r\n") <> "]")
    print(console, "[" <> string.trim("nospace") <> "]")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std string trim diverged");
        assert_eq!(compiled, vec!["[hello]", "[hi]", "[bye]", "[x]", "[nospace]"]);
    }

    // Traits: an `impl` provides a method per type, and a trait-method call
    // resolves to the impl for the receiver's concrete type — at a literal
    // receiver, a `let`-bound one, and across two implementing types. The trait
    // is lowered to ordinary functions, so both backends agree.
    #[test]
    fn traits_concrete_dispatch_backends_agree() {
        let src = r#"
trait Show:
    fn show(self) -> String

impl Show for Int:
    fn show(self) -> String:
        int_to_string(self)

impl Show for Bool:
    fn show(self) -> String:
        if self:
            "yes"
        else:
            "no"

fn main(console: Console):
    print(console, show(42))
    print(console, show(true))
    let n = 7
    print(console, show(n))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "trait dispatch diverged");
        assert_eq!(run_on_wasm(src), vec!["42", "yes", "7"]);
    }

    // Traits over a user ADT: the receiver type comes from the constructor, and
    // the impl body matches on `self`. Both backends agree.
    #[test]
    fn traits_dispatch_on_adt_backends_agree() {
        let src = r#"
type Shape:
    Circle(Int)
    Square(Int)

trait Area:
    fn area(self) -> Int

impl Area for Shape:
    fn area(self) -> Int:
        match self:
            Circle(r) -> ((r * r) * 3)
            Square(s) -> (s * s)

fn main(console: Console):
    print(console, int_to_string(area(Circle(2))))
    print(console, int_to_string(area(Square(3))))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "trait ADT dispatch diverged");
        assert_eq!(run_on_wasm(src), vec!["12", "9"]);
    }

    // Default trait methods: a method with a body in the trait is inherited by
    // impls that don't define it (calling the impl's other methods on `self`),
    // and can be overridden. Both backends agree.
    #[test]
    fn traits_default_methods_backends_agree() {
        let src = r#"
trait Label:
    fn tag(self) -> String
    fn shout(self) -> String:
        (tag(self) <> "!")

impl Label for Int:
    fn tag(self) -> String:
        "int"

impl Label for Bool:
    fn tag(self) -> String:
        "bool"

    fn shout(self) -> String:
        "BOOL!!"

fn main(console: Console):
    print(console, shout(5))
    print(console, shout(true))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "trait default-method diverged");
        assert_eq!(run_on_wasm(src), vec!["int!", "BOOL!!"]);
    }

    // Cross-module traits: a trait and its impls defined in one module are used
    // from another that imports it. Desugaring runs after linking, so the
    // generated methods and their call sites resolve across the flat merged
    // namespace. Both backends agree.
    #[test]
    fn traits_cross_module_backends_agree() {
        let show_mod = r#"
trait Show:
    fn show(self) -> String

impl Show for Int:
    fn show(self) -> String:
        int_to_string(self)

impl Show for Bool:
    fn show(self) -> String:
        if self:
            "Y"
        else:
            "N"
"#;
        let app = r#"
import show_mod

fn main(console: Console):
    print(console, show(42))
    print(console, show(false))
"#;
        let sources = [("show_mod", show_mod), ("app", app)];
        let interpreted = interpreter::run_program(&sources, "app").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "app");
        assert_eq!(interpreted, compiled, "cross-module trait diverged");
        assert_eq!(compiled, vec!["42", "N"]);
    }

    // The standard `Ord` trait: `import ord` brings comparison polymorphism into
    // scope. The built-in Int impl, a user type implementing only `compare`, and
    // the derived default methods (`less`/`greater`/`equal`) all work, and both
    // backends agree.
    #[test]
    fn std_ord_trait_backends_agree() {
        let client = r#"
import ord

type Money:
    Money(Int)

impl Ord for Money:
    fn compare(self, other: Money) -> Int:
        match self:
            Money(a) -> match other:
                Money(b) -> if (a < b): (-1) else: if (a > b): 1 else: 0

fn main(console: Console):
    print(console, int_to_string(compare(3, 5)))
    print(console, to_string(less(3, 5)))
    print(console, to_string(greater_equal(5, 5)))
    print(console, int_to_string(compare(1.5, 0.5)))
    print(console, to_string(less(1.5, 2.5)))
    print(console, int_to_string(compare(Money(10), Money(4))))
    print(console, to_string(greater(Money(10), Money(4))))
    print(console, to_string(equal(Money(7), Money(7))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std Ord diverged");
        assert_eq!(
            compiled,
            vec!["-1", "true", "true", "1", "true", "1", "true", "true"]
        );
    }

    // The standard `Show` trait: `show` renders built-in types and any user type
    // that implements it — including the rendering of a value the built-in
    // `to_string` couldn't. Both backends agree.
    #[test]
    fn std_show_trait_backends_agree() {
        let client = r#"
import show

type Point:
    Point(Int, Int)

impl Show for Point:
    fn show(self) -> String:
        match self:
            Point(x, y) -> (((("(" <> int_to_string(x)) <> ", ") <> int_to_string(y)) <> ")")

fn main(console: Console):
    print(console, show(42))
    print(console, show(true))
    print(console, show("hi"))
    print(console, show(Point(2, 3)))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std Show diverged");
        assert_eq!(compiled, vec!["42", "true", "hi", "(2, 3)"]);
    }

    // Generic bounds: `pick_max(x: a, y: a) -> a where a: Ord` is a template,
    // monomorphized per concrete instantiation; the `greater` trait call inside
    // each specialization resolves to that type's Ord impl. Exercised over Int
    // (built-in impl) and a user type. Both backends agree.
    #[test]
    fn generic_bounds_backends_agree() {
        let client = r#"
import ord

type Box:
    Box(Int)

impl Ord for Box:
    fn compare(self, other: Box) -> Int:
        match self:
            Box(a) -> match other:
                Box(b) -> if (a < b): (-1) else: if (a > b): 1 else: 0

fn pick_max(x: a, y: a) -> a where a: Ord:
    if greater(x, y):
        x
    else:
        y

fn unbox(b: Box) -> Int:
    match b:
        Box(n) -> n

fn main(console: Console):
    print(console, int_to_string(pick_max(3, 7)))
    print(console, int_to_string(pick_max(20, 5)))
    print(console, int_to_string(unbox(pick_max(Box(4), Box(11)))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "generic bounds diverged");
        assert_eq!(compiled, vec!["7", "20", "11"]);
    }

    // The stdlib's generic `Ord` helpers (max_of/min_of/clamp) are bounded
    // generics living in the `ord` module, monomorphized at the user's call
    // sites — over Int (incl. a negative literal) and a user Box type. Proves
    // cross-module bounded-generic monomorphization. Both backends agree.
    #[test]
    fn std_ord_generics_backends_agree() {
        let client = r#"
import ord

type Box:
    Box(Int)

impl Ord for Box:
    fn compare(self, other: Box) -> Int:
        match self:
            Box(a) -> match other:
                Box(b) -> if (a < b): (-1) else: if (a > b): 1 else: 0

fn unbox(b: Box) -> Int:
    match b:
        Box(n) -> n

fn main(console: Console):
    print(console, int_to_string(ord.max_of((-5), 3)))
    print(console, int_to_string(ord.min_of(8, 2)))
    print(console, int_to_string(ord.clamp(10, 0, 5)))
    print(console, int_to_string(ord.clamp(0, 3, 9)))
    print(console, int_to_string(unbox(ord.max_of(Box(4), Box(11)))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std Ord generics diverged");
        assert_eq!(compiled, vec!["3", "2", "5", "3", "11"]);
    }

    // Bounds through `List(a)`: a generic over a collection. `ord.maximum` /
    // `ord.minimum` are bounded generics taking `List(a) where a: Ord`,
    // monomorphized by the list's element type; the trait call inside resolves
    // via the for-loop variable's element type. Exercised over Int (incl. an
    // empty list -> default) and a user Box type. Both backends agree.
    #[test]
    fn generic_over_list_backends_agree() {
        let client = r#"
import ord

type Box:
    Box(Int)

impl Ord for Box:
    fn compare(self, other: Box) -> Int:
        match self:
            Box(a) -> match other:
                Box(b) -> if (a < b): (-1) else: if (a > b): 1 else: 0

fn unbox(b: Box) -> Int:
    match b:
        Box(n) -> n

fn main(console: Console):
    print(console, int_to_string(ord.maximum([3, 7, 2, 9, 4], 0)))
    print(console, int_to_string(ord.minimum([3, 7, 2, 9, 4], 100)))
    print(console, int_to_string(ord.maximum([], 42)))
    print(console, int_to_string(unbox(ord.maximum([Box(2), Box(8), Box(5)], Box(0)))))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "generic-over-list diverged");
        assert_eq!(compiled, vec!["9", "2", "42", "8"]);
    }

    // Indentation-based (off-side rule) syntax: blocks are delimited by `:` +
    // indentation rather than braces. A layout pass turns it into the brace form
    // the rest of the pipeline expects, so both backends agree — here over a
    // type, match, for-loop, let/var, and calls.
    #[test]
    fn indentation_syntax_backends_agree() {
        let src = r#"
type Shape:
    Circle(Int)
    Rect(Int, Int)

fn area(s: Shape) -> Int:
    match s:
        Circle(r) -> 3 * r * r
        Rect(w, h) -> w * h

fn main(console: Console):
    let xs = [area(Circle(2)), area(Rect(3, 4))]
    var total = 0
    for x in xs:
        total = total + x
    print(console, int_to_string(total))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "indentation backends diverged");
        assert_eq!(run_on_wasm(src), vec!["24"]);
    }

    // Indentation syntax with traits/impls and a nested if/else expression.
    #[test]
    fn indentation_traits_backends_agree() {
        let src = r#"
trait Show:
    fn show(self) -> String

impl Show for Int:
    fn show(self) -> String:
        int_to_string(self)

impl Show for Bool:
    fn show(self) -> String:
        if self:
            "yes"
        else:
            "no"

fn main(console: Console):
    print(console, show(42))
    print(console, show(true))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "indentation traits diverged");
        assert_eq!(run_on_wasm(src), vec!["42", "yes"]);
    }

    // Regression: a `(...)` expression on the line after a block must be its own
    // statement, not an application of the block's value — the virtual closing
    // brace sits on the previous line so `} (a, n)` stays two things. (This is
    // what `list.partition`'s trailing `(yes, no)` exercises.)
    #[test]
    fn indentation_block_then_paren_expr_backends_agree() {
        let src = r#"
fn pair(n: Int) -> (Int, Int):
    var a = 0
    for i in [1, 2, 3]:
        a = a + i
    (a, n)

fn main(console: Console):
    let (x, y) = pair(10)
    print(console, int_to_string(x))
    print(console, int_to_string(y))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "block-then-paren diverged");
        assert_eq!(run_on_wasm(src), vec!["6", "10"]);
    }

    // std/http: a real HTTP/1.1 GET over the Net capability against a loopback
    // server. Networking is interpreter-only (not compiled), so this isn't a
    // differential test; it proves the capability-gated socket primitives plus
    // the http library parse a live response into status + body.
    #[test]
    fn std_http_get_against_loopback() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // Drain the whole request (up to the blank header line) before
                // replying — closing with unread data would RST the client.
                let mut req = Vec::new();
                let mut tmp = [0u8; 256];
                while let Ok(n) = stream.read(&mut tmp) {
                    if n == 0 {
                        break;
                    }
                    req.extend_from_slice(&tmp[..n]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let resp = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello";
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        let program = format!(
            r#"
import http
fn main(console: Console, net: Net):
    let r = http.get(net, "127.0.0.1", {port}, "/")
    print(console, int_to_string(http.status(r)))
    print(console, http.body(r))
"#
        );
        let mods = vec![("main".to_string(), parser::parse_module(&program).expect("parse"))];
        let linked = crate::linker::link(mods, "main").expect("link");
        let out = interpreter::run_module(
            linked,
            std::path::Path::new("."),
            vec![format!("127.0.0.1:{port}")],
        )
        .expect("run");
        server.join().ok();
        assert_eq!(out, vec!["200".to_string(), "hello".to_string()]);
    }

    // A server replying with a non-numeric status code must not crash the client:
    // `status_code` guards `string_to_int` and reports 0 for a malformed status
    // line, so the body is still readable. Interpreter-only.
    #[test]
    fn std_http_tolerates_malformed_status_line() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut tmp = [0u8; 256];
                let mut req = Vec::new();
                while let Ok(n) = stream.read(&mut tmp) {
                    if n == 0 {
                        break;
                    }
                    req.extend_from_slice(&tmp[..n]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                // A non-numeric status code (`BAD`) — would trap string_to_int.
                let resp = "HTTP/1.1 BAD Weird\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi";
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        let program = format!(
            r#"
import http
fn main(console: Console, net: Net):
    let r = http.get(net, "127.0.0.1", {port}, "/")
    print(console, int_to_string(http.status(r)))
    print(console, http.body(r))
"#
        );
        let mods = vec![("main".to_string(), parser::parse_module(&program).expect("parse"))];
        let linked = crate::linker::link(mods, "main").expect("link");
        let out = interpreter::run_module(
            linked,
            std::path::Path::new("."),
            vec![format!("127.0.0.1:{port}")],
        )
        .expect("run");
        server.join().ok();
        assert_eq!(out, vec!["0".to_string(), "hi".to_string()]);
    }

    // std/http POST: send a request body and read it back from a loopback echo
    // server. Interpreter-only (networking isn't compiled).
    #[test]
    fn std_http_post_against_loopback() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = match listener.accept() {
                Ok(x) => x,
                Err(_) => return,
            };
            // Read the full request: headers, then Content-Length body bytes.
            let mut data: Vec<u8> = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                let text = String::from_utf8_lossy(&data).into_owned();
                if let Some(hdr_end) = text.find("\r\n\r\n") {
                    let clen: usize = text[..hdr_end]
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    if data.len() >= hdr_end + 4 + clen {
                        break;
                    }
                }
                match stream.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(k) => data.extend_from_slice(&tmp[..k]),
                    Err(_) => break,
                }
            }
            let text = String::from_utf8_lossy(&data);
            let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.as_bytes().len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        });
        let program = format!(
            r#"
import http
fn main(console: Console, net: Net):
    let r = http.post(net, "127.0.0.1", {port}, "/echo", "hello body")
    print(console, int_to_string(http.status(r)))
    print(console, http.body(r))
"#
        );
        let mods = vec![("main".to_string(), parser::parse_module(&program).expect("parse"))];
        let linked = crate::linker::link(mods, "main").expect("link");
        let out = interpreter::run_module(
            linked,
            std::path::Path::new("."),
            vec![format!("127.0.0.1:{port}")],
        )
        .expect("run");
        server.join().ok();
        assert_eq!(out, vec!["200".to_string(), "hello body".to_string()]);
    }

    // std/http response headers: case-insensitive lookup + a missing header.
    // Interpreter-only (networking).
    #[test]
    fn std_http_headers_against_loopback() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut req = Vec::new();
                let mut tmp = [0u8; 256];
                while let Ok(n) = stream.read(&mut tmp) {
                    if n == 0 {
                        break;
                    }
                    req.extend_from_slice(&tmp[..n]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let resp = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-Custom: abc\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi";
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        let program = format!(
            r#"
import http
import option
fn main(console: Console, net: Net):
    let r = http.get(net, "127.0.0.1", {port}, "/")
    print(console, option.unwrap_or(http.header(r, "Content-Type"), "none"))
    print(console, option.unwrap_or(http.header(r, "x-custom"), "none"))
    print(console, option.unwrap_or(http.header(r, "Missing"), "none"))
"#
        );
        let mods = vec![("main".to_string(), parser::parse_module(&program).expect("parse"))];
        let linked = crate::linker::link(mods, "main").expect("link");
        let out = interpreter::run_module(
            linked,
            std::path::Path::new("."),
            vec![format!("127.0.0.1:{port}")],
        )
        .expect("run");
        server.join().ok();
        assert_eq!(out, vec!["application/json", "abc", "none"]);
    }

    // std/json: build a nested Json value and serialize it. Pure (no
    // capabilities), so it compiles to WASM and both backends must agree.
    #[test]
    fn std_json_encode_backends_agree() {
        let client = r#"
import json
fn main(console: Console):
    let j = JsonObject([
        ("name", JsonString("witchy")),
        ("version", JsonInt(1)),
        ("tags", JsonArray([JsonString("safe"), JsonString("fast")])),
        ("stable", JsonBool(false)),
        ("extra", JsonNull)
    ])
    print(console, json.encode(j))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std json encode diverged");
        assert_eq!(
            compiled,
            vec![
                r#"{"name":"witchy","version":1,"tags":["safe","fast"],"stable":false,"extra":null}"#
            ]
        );
    }

    // std/json decode: parse JSON text then re-encode it. The round trip
    // exercises the recursive-descent parser (objects, arrays, strings, bools,
    // null, negative ints, nesting) and must agree on both backends.
    #[test]
    fn std_json_decode_roundtrip_backends_agree() {
        let client = r#"
import json
fn main(console: Console):
    let input = "{\"name\":\"witchy\",\"nums\":[1,2,3],\"ok\":true,\"nil\":null,\"neg\":-5,\"nested\":{\"a\":[true,false]}}"
    match json.decode(input):
        Ok(j) -> print(console, json.encode(j))
        Err(e) -> print(console, "error: " <> e)
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std json decode diverged");
        assert_eq!(
            compiled,
            vec![
                r#"{"name":"witchy","nums":[1,2,3],"ok":true,"nil":null,"neg":-5,"nested":{"a":[true,false]}}"#
            ]
        );
    }

    // std/json accessors: decode then pull out a string field (object key
    // lookup), an int field, and an array element. Object lookup compares the
    // decoded, heap-built key with `==`; both backends agree now that codegen
    // tracks the type of a tuple-destructured loop variable (so the comparison
    // is by content, not pointer).
    #[test]
    fn std_json_accessors_backends_agree() {
        let client = r#"
import json
import option
fn field(j: Json, k: String) -> Json:
    match json.get(j, k):
        Some(v) -> v
        None -> JsonNull

fn elem_int(j: Json, k: String, i: Int) -> Int:
    match json.index(field(j, k), i):
        Some(e) -> option.unwrap_or(json.as_int(e), 0)
        None -> 0

fn main(console: Console):
    match json.decode("{\"name\":\"witchy\",\"version\":3,\"items\":[10,20,30]}"):
        Ok(j) ->
            print(console, option.unwrap_or(json.as_string(field(j, "name")), "?"))
            print(console, int_to_string(option.unwrap_or(json.as_int(field(j, "version")), 0)))
            print(console, int_to_string(elem_int(j, "items", 1)))
        Err(e) -> print(console, e)
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std json accessors diverged");
        assert_eq!(compiled, vec!["witchy", "3", "20"]);
    }

    // Hex (0x..) and binary (0b..) integer literals, including underscore
    // separators, feeding the bitwise operators. Both backends agree.
    #[test]
    fn hex_binary_literals_backends_agree() {
        let src = r#"
fn main(console: Console):
    print(console, int_to_string(255))
    print(console, int_to_string(10))
    print(console, int_to_string((255 & 15)))
    print(console, int_to_string((12 | 3)))
    print(console, int_to_string(65535))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "hex/binary literals diverged");
        assert_eq!(run_on_wasm(src), vec!["255", "10", "15", "15", "65535"]);
    }

    #[test]
    fn string_to_int_backends_agree() {
        // string_to_int now compiles: leading whitespace and an optional sign
        // are honored, and the parsed value feeds straight into arithmetic.
        let src = r#"
fn main(console: Console):
    print(console, int_to_string(string_to_int("42")))
    print(console, int_to_string(string_to_int("-17")))
    print(console, int_to_string(string_to_int("  123  ")))
    print(console, int_to_string(string_to_int("+8")))
    print(console, int_to_string(string_to_int("0")))
    print(console, int_to_string((string_to_int("1000000") + 1)))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["42", "-17", "123", "8", "0", "1000001"]);
    }

    #[test]
    fn bitwise_not_backends_agree() {
        // ~x = -x-1 (width-independent), so it agrees across backends.
        let src = r#"
fn main(console: Console):
    print(console, int_to_string((~0)))
    print(console, int_to_string((~5)))
    print(console, int_to_string((~(0 - 1))))
    print(console, int_to_string((255 & (~15))))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["-1", "-6", "0", "240"]);
    }

    #[test]
    fn bitwise_operators_backends_agree() {
        // & | ^ << >> on Int, with precedence (& tighter than |, both tighter
        // than ==), and or-patterns still parsing (| in pattern position). Both
        // backends agree.
        let src = r#"
fn classify(n: Int) -> String:
    match n:
        1 -> "pow2"
        2 -> "pow2"
        4 -> "pow2"
        _ -> "other"

fn main(console: Console):
    print(console, int_to_string((12 & 10)))
    print(console, int_to_string((12 | 10)))
    print(console, int_to_string((12 ^ 10)))
    print(console, int_to_string((1 << 4)))
    print(console, int_to_string((256 >> 2)))
    print(console, int_to_string(((5 & 3) | 8)))
    print(console, to_string(((5 & 4) == 4)))
    print(console, classify(2))
    print(console, classify(3))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(
            run_on_wasm(src),
            vec!["8", "14", "6", "16", "64", "9", "true", "pow2", "other"]
        );
    }

    #[test]
    fn or_patterns_backends_agree() {
        // `p1 | p2 -> body` desugars to one arm per alternative. Works for
        // literal alternatives and for constructor alternatives that bind the
        // same variable. Both backends agree.
        let src = r#"
type Shape:
    Circle(Int)
    Square(Int)
    Rect(Int, Int)

fn classify(n: Int) -> String:
    match n:
        1 -> "small"
        2 -> "small"
        3 -> "small"
        4 -> "medium"
        5 -> "medium"
        _ -> "big"

fn side(s: Shape) -> Int:
    match s:
        Circle(r) -> r
        Square(r) -> r
        Rect(w, h) -> w

fn main(console: Console):
    print(console, classify(2))
    print(console, classify(5))
    print(console, classify(10))
    print(console, int_to_string(side(Circle(5))))
    print(console, int_to_string(side(Square(7))))
    print(console, int_to_string(side(Rect(3, 4))))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(
            run_on_wasm(src),
            vec!["small", "medium", "big", "5", "7", "3"]
        );
    }

    #[test]
    fn generics_example_runs_on_wasm() {
        // A generic `swap((a, b)) -> (b, a)` on a mixed (Int, String) tuple:
        // tuple pattern match + construction through a generic function.
        let src = include_str!("../examples/generics.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["answer", "42"]);
    }

    #[test]
    fn signs_example_runs_on_wasm() {
        // Negative-literal match patterns (`-1 -> ...`).
        let src = include_str!("../examples/signs.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["left", "right", "stay", "?"]);
    }

    #[test]
    fn nested_scope_shadowing_backends_agree() {
        // An inner binding that shadows an outer one of the same name must not
        // clobber the outer: after the inner scope ends, the outer value is back.
        let src = r#"
fn main(console: Console):
    let x = 1
    if true:
        let x = 2
        print(console, int_to_string(x))
    print(console, int_to_string(x))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["2", "1"]);
    }

    #[test]
    fn record_with_collection_field_backends_agree() {
        // A record holding a List(Int) and a String: field access, length on a
        // list field, and a `for` loop iterating a list *field* (the iterand is a
        // field expression, not a variable). Both backends agree.
        let src = r#"
type Bag:
    items: List(Int)
    label: String

fn main(console: Console):
    let b = Bag([10, 20, 30], "nums")
    print(console, (b).label)
    print(console, int_to_string(length((b).items)))
    var total = 0
    for x in (b).items:
        total = (total + x)
    print(console, int_to_string(total))
    print(console, int_to_string(at((b).items, 1)))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["nums", "3", "60", "20"]);
    }

    #[test]
    fn nested_records_backends_agree() {
        // A record containing a record: chained field access (o.inner.v), nested
        // construction, `update` on a nested field, and immutability of the
        // original. Both backends must agree.
        let src = r#"
type Inner:
    v: Int

type Outer:
    name: String
    inner: Inner

fn main(console: Console):
    let o = Outer("x", Inner(42))
    print(console, int_to_string(((o).inner).v))
    let o2 = Outer(inner: Inner((((o).inner).v + 1)), ..o)
    print(console, int_to_string(((o2).inner).v))
    print(console, (o).name)
    print(console, int_to_string(((o).inner).v))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["42", "43", "x", "42"]);
    }

    #[test]
    fn inout_swap_and_loop_backends_agree() {
        // Harder `inout`: two inout parameters (swap) — exercising move-out of
        // multiple values — and an inout mutation inside a loop. Both backends
        // must agree.
        let src = r#"
fn swap(inout a: Int, inout b: Int):
    let t = a
    a = b
    b = t

fn bump_by(inout n: Int, d: Int):
    n = (n + d)

fn main(console: Console):
    var x = 3
    var y = 8
    swap(x, y)
    print(console, int_to_string(x))
    print(console, int_to_string(y))
    var acc = 0
    var i = 1
    while (i < 5):
        bump_by(acc, i)
        i = (i + 1)
    print(console, int_to_string(acc))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        // And the concrete values, to be sure both compute the right thing.
        assert_eq!(run_on_wasm(src), vec!["8", "3", "10"]);
    }

    #[test]
    fn mutate_example_runs_on_wasm() {
        // `inout` (move-in / move-out) compiles: the example agrees with the
        // interpreter through the WASM backend.
        let src = include_str!("../examples/mutate.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
    }

    #[test]
    fn ownership_example_runs_on_wasm() {
        // `sink` (consume / move ownership) compiles and agrees across backends.
        let src = include_str!("../examples/ownership.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
    }

    #[test]
    fn actors_example() {
        assert_eq!(
            interp(include_str!("../examples/actors.witchy")),
            vec!["[1] direct message", "[2] another direct", "[3] relayed message"]
        );
    }

    #[test]
    fn commands_example_runs_and_compiles() {
        let src = include_str!("../examples/commands.witchy");
        assert_eq!(interp(src), vec!["total is 1"]);
        assert_fn_compiles(src);
    }

    #[test]
    fn files_example_reads_through_capability() {
        // Run from the crate root so examples/data/greeting.txt resolves.
        assert_eq!(
            interp(include_str!("../examples/files.witchy")),
            vec!["hello from a sandboxed Dir capability"]
        );
    }

    #[test]
    fn runs_a_file_with_file_based_imports() {
        let dir = std::env::temp_dir().join(format!("witchy_cli_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("strutil.witchy"),
            r#"
fn shout(s: String) -> String:
    ("HI " <> s)
"#,
        )
        .unwrap();
        let app = dir.join("app.witchy");
        std::fs::write(
            &app,
            "import strutil\nfn main(console: Console):\n    print(console, strutil.shout(\"x\"))\n",
        )
        .unwrap();

        let out = crate::execute_file(app.to_str().unwrap(), Vec::new()).unwrap();
        assert_eq!(out, vec!["HI x"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn caps_diff_gate_flags_a_widening_across_versions() {
        // The supply-chain gate end-to-end: a dependency update whose public API
        // newly demands `Net` is reported as a widening (so CI/install can block),
        // while an unchanged footprint is not.
        let dir = std::env::temp_dir().join(format!("witchy_capsdiff_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let v1 = dir.join("v1.witchy");
        let v2 = dir.join("v2.witchy");
        std::fs::write(&v1, r#"
pub fn serve(console: Console) -> Int:
    0
"#).unwrap();
        std::fs::write(
            &v2,
            r#"
pub fn serve(console: Console, net: Net) -> Int:
    0
"#,
        )
        .unwrap();
        assert!(
            crate::report_capability_diff(v1.to_str().unwrap(), v2.to_str().unwrap()).unwrap(),
            "newly demanding Net must be flagged as a widening"
        );
        assert!(
            !crate::report_capability_diff(v1.to_str().unwrap(), v1.to_str().unwrap()).unwrap(),
            "an unchanged footprint must not be a widening"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tuples_example() {
        assert_eq!(
            interp(include_str!("../examples/tuples.witchy")),
            vec!["3 remainder 2"]
        );
    }

    #[test]
    fn generics_example() {
        assert_eq!(
            interp(include_str!("../examples/generics.witchy")),
            vec!["answer", "42"]
        );
    }

    #[test]
    fn result_example() {
        assert_eq!(
            interp(include_str!("../examples/result.witchy")),
            vec!["ok 5", "err divide by zero"]
        );
    }

    #[test]
    fn try_example() {
        assert_eq!(
            interp(include_str!("../examples/try.witchy")),
            vec!["= 11", "error: divide by zero", "error: divide by zero"]
        );
    }

    #[test]
    fn loops_example() {
        assert_eq!(
            interp(include_str!("../examples/loops.witchy")),
            vec!["sum = 108", "witchy loops work"]
        );
    }

    #[test]
    fn listmatch_example() {
        assert_eq!(
            interp(include_str!("../examples/listmatch.witchy")),
            vec!["sum = 21", "starts with 3", "one: 42", "empty"]
        );
    }

    #[test]
    fn records_example() {
        assert_eq!(
            interp(include_str!("../examples/records.witchy")),
            vec![
                "origin.x = 2",
                "moved = (12, 3)",
                "manhattan(moved) = 15"
            ]
        );
    }

    #[test]
    fn record_update_example() {
        assert_eq!(
            interp(include_str!("../examples/record_update.witchy")),
            vec!["alice 100", "alice 150", "alice smith 150"]
        );
    }

    #[test]
    fn eval_example() {
        assert_eq!(interp(include_str!("../examples/eval.witchy")), vec!["20"]);
    }

    #[test]
    fn bank_example() {
        assert_eq!(
            interp(include_str!("../examples/bank.witchy")),
            vec![
                "total = 150",
                "remaining: 90",
                "error: insufficient funds for bob"
            ]
        );
    }

    #[test]
    fn higher_order_example() {
        assert_eq!(
            interp(include_str!("../examples/higher_order.witchy")),
            vec!["15", "81", "15", "120"]
        );
    }

    #[test]
    fn list_ops_example() {
        assert_eq!(
            interp(include_str!("../examples/list_ops.witchy")),
            vec!["55", "6", "0-2-4"]
        );
    }

    #[test]
    fn wordcount_example() {
        assert_eq!(
            interp(include_str!("../examples/wordcount.witchy")),
            vec!["3", "1", "0", "4"]
        );
    }

    #[test]
    fn inventory_example() {
        assert_eq!(
            interp(include_str!("../examples/inventory.witchy")),
            vec!["total = 9", "over 2: 2"]
        );
    }

    #[test]
    fn guard_example() {
        assert_eq!(
            interp(include_str!("../examples/guard.witchy")),
            vec!["negative", "zero", "positive", "8", "-1"]
        );
    }

    #[test]
    fn signs_example() {
        assert_eq!(
            interp(include_str!("../examples/signs.witchy")),
            vec!["left", "right", "stay", "?"]
        );
    }

    #[test]
    fn parse_kv_example() {
        assert_eq!(
            interp(include_str!("../examples/parse_kv.witchy")),
            vec!["timeout", "30", "true"]
        );
    }

    #[test]
    fn fizzbuzz_example() {
        assert_eq!(
            interp(include_str!("../examples/fizzbuzz.witchy")),
            vec![
                "1", "2", "Fizz", "4", "Buzz", "Fizz", "7", "8", "Fizz", "Buzz", "11", "Fizz",
                "13", "14", "FizzBuzz"
            ]
        );
    }

    #[test]
    fn compute_example_compiles() {
        assert_fn_compiles(include_str!("../examples/compute.witchy"));
    }

    #[test]
    fn strings_example_compiles() {
        assert_fn_compiles(include_str!("../examples/strings.witchy"));
    }

    #[test]
    fn shapes_example_compiles() {
        assert_fn_compiles(include_str!("../examples/shapes.witchy"));
    }

    #[test]
    fn counter_example_compiles() {
        assert_actor_compiles(include_str!("../examples/counter.witchy"));
    }
}

/// Compile a witchy program to WASM and run it on the runtime, demonstrating
/// that the capability gate now applies to *compiled* witchy: granted, it runs;
/// ungranted, the module cannot instantiate.
fn run_compiled(rt: &mut Runtime, title: &str, program: &str) {
    println!("\n== {title} ==");
    if let Err(e) = typeck::check_str(program) {
        println!("{e}");
        return;
    }
    let module = match parser::parse_module(program) {
        Ok(m) => m,
        Err(e) => {
            println!("{e}");
            return;
        }
    };
    let wat = match codegen::compile_module(&module) {
        Ok(w) => w,
        Err(e) => {
            println!("{e}");
            return;
        }
    };

    // Granted the output capabilities: the compiled module runs and prints.
    match rt.spawn(
        wat.as_bytes(),
        Capabilities {
            print: true,
            print_int: true,
            ..Default::default()
        },
        4,
    ) {
        Ok(mut actor) => {
            if let Err(e) = actor.run() {
                println!("error: {e}");
            }
        }
        Err(e) => println!("spawn failed: {e}"),
    }

    // Denied: the same compiled module cannot even instantiate.
    match rt.spawn(wat.as_bytes(), Capabilities::none(), 4) {
        Ok(_) => println!("!! SECURITY FAILURE: compiled module ran without the capability"),
        Err(e) => println!("DENIED without capability (as designed): {e}"),
    }
}

/// Link a multi-module program, compile the flat module to WASM, and run it on
/// its own VM — so an imported library (e.g. `list`) is genuinely compiled, not
/// just the entry module.
fn run_compiled_program(rt: &mut Runtime, title: &str, sources: &[(&str, &str)], entry: &str) {
    println!("\n== {title} ==");
    let mods: Result<Vec<(String, ast::Module)>, String> = sources
        .iter()
        .map(|(n, s)| {
            parser::parse_module(s)
                .map(|m| ((*n).to_string(), m))
                .map_err(|e| e.to_string())
        })
        .collect();
    let linked = match mods.and_then(|m| linker::link(m, entry).map_err(|e| e.to_string())) {
        Ok(m) => m,
        Err(e) => {
            println!("{e}");
            return;
        }
    };
    if let Err(e) = typeck::check(&linked) {
        println!("{e}");
        return;
    }
    let wat = match codegen::compile_module(&linked) {
        Ok(w) => w,
        Err(e) => {
            println!("{e}");
            return;
        }
    };
    match rt.spawn(
        wat.as_bytes(),
        Capabilities {
            print: true,
            print_int: true,
            ..Default::default()
        },
        4,
    ) {
        Ok(mut actor) => {
            if let Err(e) = actor.run() {
                println!("error: {e}");
            }
        }
        Err(e) => println!("spawn failed: {e}"),
    }
}

fn run_witchy(title: &str, program: &str) {
    println!("\n== {title} ==");
    if let Err(e) = typeck::check_str(program) {
        println!("{e}");
        return;
    }
    match interpreter::run(program) {
        Ok(output) => {
            for line in output {
                println!("{line}");
            }
        }
        Err(e) => println!("error: {e}"),
    }
}

