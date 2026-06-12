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
#![allow(clippy::collapsible_if, clippy::collapsible_match, clippy::items_after_test_module)]

mod actor_system;
mod analysis;
mod aliases;
mod ast;
mod capabilities;
mod codegen;
mod consts;
mod comptime;
mod derive;
mod doc;
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
                                                  (a verify-the-compiler tool, not a workflow step)
    witchy test     <file.witchy|dir>             run in-language tests (zero-param `test_*` functions)
    witchy sandbox [--dir <root>] [--net <addr>]... <file.witchy> [args...]
                                                  compile and run in a VM granted exactly its footprint
    witchy emit-wat <file.witchy>                 print the compiled WebAssembly text (the module sandbox runs)
    witchy caps     [file.witchy]                 report the capability footprint (defaults to the project entry)
    witchy caps-diff <old.witchy> <new.witchy>    fail if the footprint widened
    witchy which    <name>                        find a function in the standard library by (partial) name
    witchy fmt [--check] <file.witchy>            reformat in place (--check: verify only, exit 1 if not)
    witchy lsp                                    run the language server
    witchy demo                                   built-in capability/runtime demonstration

Package commands: new, init, add, build, run [args...], update, audit, tree,
outdated, why, why-cap, verify, vendor, publish, promote, yank, list — run
`witchy coven` for the full package-manager help. All of them accept
`-C <dir>`; `witchy run` passes everything after `run` (or after `--`) to the
program as `main`'s `args`, including `--help`."
    );
}

fn main() -> wasmtime::Result<()> {
    // `witchy doc <file>...` prints Markdown API docs (one section per file) to
    // stdout — public functions, their signatures, and their doc comments.
    if std::env::args().nth(1).as_deref() == Some("doc") {
        use std::path::Path;
        let files: Vec<String> = std::env::args().skip(2).collect();
        if files.is_empty() {
            eprintln!("usage: witchy doc <file.witchy>...");
            std::process::exit(2);
        }
        let mut out = String::from("# API reference\n\n");
        for f in &files {
            let stem = Path::new(f)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(f);
            let src = match std::fs::read_to_string(f) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("cannot read `{f}`: {e}");
                    std::process::exit(1);
                }
            };
            match doc::render(stem, &src) {
                Ok(md) => out.push_str(&md),
                Err(e) => {
                    eprintln!("{f}: {e}");
                    std::process::exit(1);
                }
            }
        }
        print!("{out}");
        return Ok(());
    }
    // `witchy which <name>` — where does a function live in the standard
    // library? Exact (or substring) matches print module-qualified signatures
    // with their doc line; a near-miss falls back to the closest name.
    if std::env::args().nth(1).as_deref() == Some("which") {
        let Some(name) = std::env::args().nth(2) else {
            eprintln!("usage: witchy which <function-name>");
            std::process::exit(1);
        };
        // A module name lists that module's exports.
        if linker::STD_MODULES.contains(&name.as_str()) {
            for s in linker::module_exports(&name) {
                println!("{s}");
            }
            return Ok(());
        }
        let sigs = linker::std_signatures(&name);
        if !sigs.is_empty() {
            for s in sigs {
                println!("{s}");
            }
            return Ok(());
        }
        match linker::closest_std_function(&name) {
            Some((f, m)) => {
                println!("no std function named `{name}` — did you mean `{m}.{f}`?");
                for s in linker::std_signatures(&f) {
                    println!("{s}");
                }
                return Ok(());
            }
            None => {
                eprintln!("no std function matches `{name}`");
                std::process::exit(1);
            }
        }
    }
    // `witchy caps [file]` reports the host-capability footprint. With no
    // file, inside a project, it analyzes the project's entry module (the
    // whole dependency tree's footprint is `witchy audit`).
    if std::env::args().nth(1).as_deref() == Some("caps") {
        let path = match std::env::args().nth(2) {
            Some(p) => p,
            None => match pm::project_entry_file() {
                Some(p) => p,
                None => {
                    eprintln!(
                        "usage: witchy caps <file>  (or run inside a project — no witchy.toml here)"
                    );
                    std::process::exit(1);
                }
            },
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
    // `witchy build-step <file> [--out <dir>] [--read <dir>] [--env <KEY>]...`
    // runs a rune's `build` entrypoint under confined grants and reports the
    // source it generated. The build step can only use the build capabilities it
    // is granted here (it cannot forge a runtime cap), so this is the build-time
    // half of the capability model, exercised in isolation.
    if std::env::args().nth(1).as_deref() == Some("build-step") {
        let mut out_dir: Option<std::path::PathBuf> = None;
        let mut read_roots: Vec<std::path::PathBuf> = Vec::new();
        let mut env_keys: Vec<String> = Vec::new();
        let mut exec_tools: Vec<String> = Vec::new();
        let mut path: Option<String> = None;
        let mut argv = std::env::args().skip(2);
        while let Some(a) = argv.next() {
            match a.as_str() {
                "--out" => out_dir = argv.next().map(std::path::PathBuf::from),
                "--read" => {
                    if let Some(d) = argv.next() {
                        read_roots.push(std::path::PathBuf::from(d));
                    }
                }
                "--env" => {
                    if let Some(k) = argv.next() {
                        env_keys.push(k);
                    }
                }
                "--exec" => {
                    if let Some(t) = argv.next() {
                        exec_tools.push(t);
                    }
                }
                _ if path.is_none() => path = Some(a),
                _ => {}
            }
        }
        let Some(path) = path else {
            eprintln!("usage: witchy build-step <file.witchy> [--out <dir>] [--read <dir>]... [--env <KEY>]... [--exec <tool>]...");
            std::process::exit(1);
        };
        match run_build_step_file(&path, out_dir, read_roots, env_keys, exec_tools) {
            Ok(files) if files.is_empty() => println!("{path}: no `build` entrypoint, or it generated no files"),
            Ok(files) => {
                println!("build step generated {} file(s):", files.len());
                for f in files {
                    println!("  {f}");
                }
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return Ok(());
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
    // `witchy test <file|dir>` runs in-language tests: zero-parameter `test_*`
    // functions, passing unless they abort (std/testing's assertions).
    if std::env::args().nth(1).as_deref() == Some("test") {
        let Some(path) = std::env::args().nth(2) else {
            eprintln!("usage: witchy test <file.witchy|dir>");
            std::process::exit(1);
        };
        match run_tests(&path) {
            Ok(true) => return Ok(()),
            Ok(false) => std::process::exit(1),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
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
    // `witchy sandbox [--dir <root>] [--net <host:port>]... <file> [args...]`
    // compiles the program to WASM and runs it in the capability-sandboxed VM,
    // granted exactly its computed footprint. `--dir` picks the subtree backing
    // a granted Dir (default `.`); each `--net` allowlists an address.
    if std::env::args().nth(1).as_deref() == Some("sandbox") {
        let mut dir_root: Option<std::path::PathBuf> = None;
        let mut net_allow: Vec<String> = Vec::new();
        let mut signing_key: Option<[u8; 32]> = None;
        let mut path: Option<String> = None;
        let mut prog_args: Vec<String> = Vec::new();
        let mut argv = std::env::args().skip(2);
        while let Some(a) = argv.next() {
            match a.as_str() {
                "--dir" if path.is_none() => match argv.next() {
                    Some(root) => dir_root = Some(std::path::PathBuf::from(root)),
                    None => {
                        eprintln!("--dir needs a directory");
                        std::process::exit(1);
                    }
                },
                "--net" if path.is_none() => match argv.next() {
                    Some(addr) => net_allow.push(addr),
                    None => {
                        eprintln!("--net needs a host:port");
                        std::process::exit(1);
                    }
                },
                "--signing-key" if path.is_none() => match argv.next() {
                    Some(file) => match load_signing_seed(&file) {
                        Ok(seed) => signing_key = Some(seed),
                        Err(e) => {
                            eprintln!("--signing-key: {e}");
                            std::process::exit(1);
                        }
                    },
                    None => {
                        eprintln!("--signing-key needs a <seed-file>");
                        std::process::exit(1);
                    }
                },
                _ if path.is_none() => path = Some(a),
                _ => prog_args.push(a),
            }
        }
        let Some(path) = path else {
            eprintln!("usage: witchy sandbox [--dir <root>] [--net <host:port>]... [--signing-key <seed-file>] <file.witchy> [args...]");
            std::process::exit(1);
        };
        match run_file_sandboxed(&path, dir_root, net_allow, prog_args, signing_key) {
            Ok((lines, exit_code)) => {
                for line in lines {
                    println!("{line}");
                }
                if let Some(code) = exit_code {
                    if code != 0 {
                        std::process::exit(code);
                    }
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
                // Grant the root `Secret` from a file holding a 64-hex-char
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
    let mut greeter = rt.spawn(GREETER, Capabilities { print: true, ..Default::default() }, 4)?;
    greeter.run()?;

    // Granted nothing — must fail to instantiate because `witchy.print` is not
    // linked into its VM.
    match rt.spawn(MALICIOUS, Capabilities::none(), 4) {
        Ok(_) => println!("!! SECURITY FAILURE: ungranted actor was allowed to instantiate"),
        Err(e) => println!("DENIED (as designed): {e}"),
    }

    println!("\n== M3: message passing across isolated VMs ==");

    // Logger can print + recv, but cannot send.
    let mut logger = rt.spawn(LOGGER, Capabilities { print: true, ..Default::default() }, 4)?;
    // Sender can only send.
    let mut sender = rt.spawn(
        sender_src(logger.id),
        Capabilities { send: true, ..Default::default() },
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
    run_actor_system(
        "witchy compiled actor system (driver + spawned VMs)",
        include_str!("../examples/dispatch.witchy"),
    );
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
        "witchy float math (sqrt + float_abs/float_min/float_max)",
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
    print(console, to_string(result.unwrap_or(compute(10, 2), (0 - 1))))
    print(console, to_string(result.unwrap_or(compute(10, 0), (0 - 1))))
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
    typeck::check(&linked).map_err(|e| e.to_string())?;
    // Performance notes from the uniqueness analysis: accumulation that
    // reverts to the copying path inside a loop (O(n²)). Never an error —
    // the copying path IS the semantics — but the cliff should be visible
    // at check time, not at a memory-cap trap.
    for (func, c) in analysis::module_cliffs(&linked) {
        // Linked-in modules' cliffs belong to their own files, not this one.
        if func.contains('.') {
            continue;
        }
        eprintln!(
            "note: in `{func}` (line {}): `{}` is rebuilt by copy on every \
             iteration of this loop — it is {}",
            c.line, c.var, c.reason
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
/// parameter; `signing_key` grants the root `Secret` capability.
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
/// A failed in-language test: its (qualified) name and the abort message.
type TestFailure = (String, String);

/// Discover and run the tests in a source file: every ZERO-parameter function
/// named `test_*`, each invoked through a synthesized `main` in a fresh
/// interpreter. A test passes by returning and fails by aborting (which
/// `std/testing`'s assertions do, with a message). Tests take no capabilities,
/// so a suite provably has no effects. Returns the failures as (name, message).
fn run_tests_in_file(path: &str) -> Result<(Vec<String>, Vec<TestFailure>), String> {
    use std::path::Path;
    let (linked, _stem) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    // Post-link names are module-qualified (`suite.test_x`); match on the bare
    // name, call the qualified one.
    let tests: Vec<String> = linked
        .items
        .iter()
        .filter_map(|it| match it {
            ast::Item::Function(f)
                if f.name.rsplit('.').next().unwrap_or(&f.name).starts_with("test_")
                    && f.params.is_empty() =>
            {
                Some(f.name.clone())
            }
            _ => None,
        })
        .collect();
    let root = Path::new(path).parent().unwrap_or(Path::new("."));
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    for test in tests {
        // Synthesize `fn main(): <test>()` (replacing any real main) and run.
        // The test name is already linker-qualified (`suite.test_x`), which the
        // parser would read as a method call — so parse a placeholder and patch
        // the call name in the AST.
        let mut m = linked.clone();
        m.items
            .retain(|it| !matches!(it, ast::Item::Function(f) if f.name == "main"));
        let mut driver = parser::parse_module("fn main():\n    witchy_test_target()\n")
            .map_err(|e| e.to_string())?;
        for it in &mut driver.items {
            if let ast::Item::Function(f) = it {
                if let Some(ast::Stmt::Expr(ast::Expr::Call { name, .. })) = f.body.stmts.first_mut()
                {
                    *name = test.clone();
                }
            }
        }
        m.items.extend(driver.items);
        match interpreter::run_module(m, root, Vec::new()) {
            Ok(_) => passed.push(test),
            Err(e) => failed.push((test, e.message)),
        }
    }
    Ok((passed, failed))
}

/// `witchy test <file|dir>`: run in-language tests, print a cargo-style
/// report, and return whether everything passed.
fn run_tests(path: &str) -> Result<bool, String> {
    let mut files: Vec<String> = Vec::new();
    let meta = std::fs::metadata(path).map_err(|e| format!("cannot read `{path}`: {e}"))?;
    if meta.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(path)
            .map_err(|e| format!("cannot read `{path}`: {e}"))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("witchy"))
            .collect();
        entries.sort();
        files.extend(entries.into_iter().filter_map(|p| p.to_str().map(String::from)));
    } else {
        files.push(path.to_string());
    }
    let mut total_pass = 0usize;
    let mut total_fail = 0usize;
    for file in &files {
        let (passed, failed) = run_tests_in_file(file)?;
        if passed.is_empty() && failed.is_empty() {
            continue;
        }
        println!("running {} test(s) in {file}", passed.len() + failed.len());
        for name in &passed {
            println!("test {name} ... ok");
        }
        for (name, msg) in &failed {
            println!("test {name} ... FAILED: {msg}");
        }
        total_pass += passed.len();
        total_fail += failed.len();
    }
    println!(
        "\ntest result: {}. {total_pass} passed; {total_fail} failed",
        if total_fail == 0 { "ok" } else { "FAILED" }
    );
    Ok(total_fail == 0)
}

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
    // An actor program runs on the compiled ACTOR SYSTEM (its main in a driver
    // VM, each spawned actor in its own); a plain program on the single-module
    // WASM runtime.
    let has_actors = linked.items.iter().any(|it| matches!(it, ast::Item::Actor(_)));
    let compiled_system = if has_actors {
        let (driver, actors, sigs, specs) = codegen::compile_system(&linked)
            .map_err(|e| format!("cannot compile to WASM (an interpreter-only feature?): {e}"))?;
        Some(
            actor_system::System::run_program(&driver, &actors, sigs, specs, &actor_system::dev_caps())
                .map_err(|e| e.to_string()),
        )
    } else {
        None
    };
    let wat = if has_actors {
        String::new()
    } else {
        codegen::compile_module(&linked)
            .map_err(|e| format!("cannot compile to WASM (an interpreter-only feature?): {e}"))?
    };
    // Run BOTH backends regardless of either failing: a program that errors on
    // one backend but produces a value on the other is itself a divergence (a
    // trap and a clean result are not the same behavior), so we must not return
    // early on the interpreter's error before observing what WASM does.
    // The compiled run export surfaces an Int-returning `main` as a final
    // print_int line (the sandbox/CLI turn it into the process exit code); the
    // interpreter's output has no such line, so drop it before comparing.
    let main_returns_int = linked.items.iter().any(|it| {
        matches!(it, ast::Item::Function(f) if f.name == "main"
            && matches!(&f.ret, Some(ast::Type::Named(n, _)) if n == "Int"))
    });
    let interp = interpreter::run_module(linked, Path::new("."), Vec::new()).map_err(|e| e.to_string());
    let compiled = match compiled_system {
        Some(result) => result,
        None => run_wat_capture(&wat).map(|mut lines| {
            if main_returns_int {
                lines.pop();
            }
            lines
        }),
    };
    match (interp, compiled) {
        (Ok(i), Ok(c)) if i == c => {
            println!(
                "\u{2713} {path}: interpreter and compiled WASM agree ({} line(s) of output)",
                i.len()
            );
            Ok(())
        }
        (Ok(i), Ok(c)) => Err(format!(
            "\u{2717} {path}: the two backends DIVERGE\n  interpreter: {i:?}\n  compiled:    {c:?}"
        )),
        // Both fail: they agree on rejecting this input (the messages differ — a
        // readable interpreter error vs. a WASM trap — but the behavior matches).
        (Err(_), Err(_)) => {
            println!("\u{2713} {path}: interpreter and compiled WASM agree (both error)");
            Ok(())
        }
        (Ok(i), Err(c)) => Err(format!(
            "\u{2717} {path}: the two backends DIVERGE\n  interpreter: Ok({i:?})\n  compiled:    Err({c})"
        )),
        (Err(i), Ok(c)) => Err(format!(
            "\u{2717} {path}: the two backends DIVERGE\n  interpreter: Err({i})\n  compiled:    Ok({c:?})"
        )),
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

/// Compile a program and run it in the WASM VM granted EXACTLY its computed
/// footprint: each capability kind in the footprint maps to its host-import
/// family, with `Dir`/`Net` rights narrowing which operations are linked. The
/// `Dir` root and `Net` allowlist are host policy (the `--dir`/`--net` flags);
/// the program's footprint decides whether they are granted at all.
fn run_file_sandboxed(
    path: &str,
    dir_root: Option<std::path::PathBuf>,
    net_allow: Vec<String>,
    args: Vec<String>,
    signing_key: Option<[u8; 32]>,
) -> Result<(Vec<String>, Option<i32>), String> {
    use crate::runtime::{Capabilities, Runtime};
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
    if footprint.total.contains_key("Secret") && signing_key.is_none() {
        return Err(format!(
            "`{path}` needs a Secret, but the host granted none (provide `--signing-key <seed-file>`)"
        ));
    }
    let has_actors = linked.items.iter().any(|it| matches!(it, ast::Item::Actor(_)));
    eprintln!(
        "sandboxing `{path}` \u{2014} granted exactly: {}",
        capabilities::show_caps(&footprint.total)
    );
    // Quiet: the captured lines are printed once by the caller (and an
    // Int-returning `main` surfaces its value as the LAST line, which the
    // caller turns into the process exit code, like the interpreter CLI).
    let mut caps = Capabilities {
        print: true,
        print_int: true,
        quiet: true,
        args,
        ..Default::default()
    };
    if footprint.total.contains_key("Clock") {
        caps.clock = true;
    }
    if footprint.total.contains_key("Env") {
        caps.env = true;
    }
    if let Some(rights) = footprint.total.get("Dir") {
        caps.dir_root = Some(dir_root.unwrap_or_else(|| std::path::PathBuf::from(".")));
        caps.dir_read = rights.contains("Read");
        caps.dir_write = rights.contains("Write");
    }
    if let Some(rights) = footprint.total.get("Net") {
        caps.net_allow = Some(net_allow);
        caps.net_connect = rights.contains("Connect");
        caps.net_listen = rights.contains("Listen");
    }
    if footprint.total.contains_key("Secret") {
        caps.signing_key = signing_key;
    }
    // An ACTOR program runs on the compiled actor system: the driver VM gets
    // the computed-footprint grant (above), and each spawned actor's VM links
    // only what its own capability fields entitle it to, with Dir/Net handles
    // translated at spawn. A plain program runs in the single-module VM.
    let mut lines = if has_actors {
        let (driver, actors, sigs, specs) = codegen::compile_system(&linked)
            .map_err(|e| format!("cannot compile to WASM (an interpreter-only feature?): {e}"))?;
        actor_system::System::run_program(&driver, &actors, sigs, specs, &caps)
            .map_err(|e| e.root_cause().to_string())?
    } else {
        let wat = codegen::compile_module(&linked)
            .map_err(|e| format!("cannot compile to WASM (an interpreter-only feature?): {e}"))?;
        let mut rt = Runtime::batch().map_err(|e| e.to_string())?;
        let mut actor = rt
            .spawn(wat.as_bytes(), caps, RUN_MEMORY_PAGES)
            .map_err(|e| e.to_string())?;
        // Surface the *root cause*, not wasmtime's outer "error while executing at
        // wasm backtrace…" wrapper: a confinement violation then reads as the same
        // clean "`..` escapes the Dir capability" both backends
        // print, and a genuine trap reads as "wasm trap: …" rather than a stack dump.
        actor.run().map_err(|e| e.root_cause().to_string())?;
        actor.output()
    };
    // At the process boundary an Int-returning `main` is the exit code (the
    // run export surfaces it as the final print_int line; pop and convert).
    let main_returns_int = linked.items.iter().any(|it| {
        matches!(it, ast::Item::Function(f) if f.name == "main"
            && matches!(&f.ret, Some(ast::Type::Named(n, _)) if n == "Int"))
    });
    let exit_code = if main_returns_int {
        lines.pop().and_then(|s| s.parse::<i32>().ok())
    } else {
        None
    };
    Ok((lines, exit_code))
}

/// Run a `build` step in the **zero-ambient WASM sandbox**: compile it (the
/// `build` entrypoint becomes the `run` export), then instantiate under a
/// `Capabilities` granting *only* the build output sandbox and read roots — so
/// the module physically has no `dir_*`/`net_*`/`print` import to call, and a
/// `..` write traps via the same confinement as a runtime `Dir`. Returns the
/// generated source files written into `out_dir`.
///
/// Used for deterministic steps (BuildOut/BuildRead only). It is hard isolation
/// for untrusted codegen logic: a bug in the interpreter could not help a build
/// step here, because the dangerous host functions simply are not linked.
pub fn run_build_step_sandboxed(
    module: ast::Module,
    out_dir: std::path::PathBuf,
    read_roots: Vec<std::path::PathBuf>,
) -> Result<Vec<String>, String> {
    use runtime::{Capabilities, Runtime};
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("build: output dir: {e}"))?;
    let wat = codegen::compile_build_module(&module).map_err(|e| e.message)?;
    let caps = Capabilities {
        build_out: Some(out_dir.clone()),
        build_read_roots: read_roots,
        ..Default::default()
    };
    let mut rt = Runtime::batch().map_err(|e| e.to_string())?;
    let mut actor = rt
        .spawn(wat.as_bytes(), caps, RUN_MEMORY_PAGES)
        .map_err(|e| e.to_string())?;
    actor.run().map_err(|e| e.root_cause().to_string())?;
    let mut generated: Vec<String> = std::fs::read_dir(&out_dir)
        .map_err(|e| format!("build: reading output dir: {e}"))?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    generated.sort();
    Ok(generated)
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
            // The dev/differential path mirrors the interpreter's automatic
            // grants: output plus the read-only ambient capabilities (Clock/Env)
            // a `main` may declare. The `sandbox` command is the strict path
            // that grants exactly the computed footprint.
            Capabilities {
                print: true,
                print_int: true,
                quiet: true,
                clock: true,
                env: true,
                dir_root: Some(std::path::PathBuf::from(".")),
                dir_read: true,
                dir_write: true,
                net_allow: Some(Vec::new()),
                net_connect: true,
                net_listen: true,
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

/// Parse, link, type-check, and run a file's `build` entrypoint under confined
/// grants, returning the names of the files it generated. The output directory
/// defaults to `./build-out`.
fn run_build_step_file(
    path: &str,
    out_dir: Option<std::path::PathBuf>,
    read_roots: Vec<std::path::PathBuf>,
    env_keys: Vec<String>,
    exec_tools: Vec<String>,
) -> Result<Vec<String>, String> {
    let (linked, _) = link_file(path)?;
    typeck::check(&linked).map_err(|e| e.to_string())?;
    let grants = interpreter::BuildGrants {
        out_dir: out_dir.unwrap_or_else(|| std::path::PathBuf::from("build-out")),
        read_roots,
        env_keys,
        exec_tools,
        ..Default::default()
    };
    interpreter::run_build_step(linked, grants).map_err(|e| e.message)
}

/// Print the host-capability footprint of a single source file: every
/// capability-touching function (entry points and private helpers), and the
/// union over the entry points.
fn report_capabilities(path: &str) -> Result<(), String> {
    let fp = analyze_file(path)?;
    let show = capabilities::show_caps;
    println!("Host-capability footprint of {path}:");
    let width = fp
        .per_function
        .iter()
        .map(|e| e.name.len())
        .max()
        .unwrap_or(0)
        .max("total".len());
    for e in &fp.per_function {
        let refined = if e.brands.is_empty() {
            String::new()
        } else {
            let names: Vec<&str> = e.brands.iter().map(String::as_str).collect();
            format!("  (refined: {})", names.join(", "))
        };
        println!("  {:<width$}  {}{}", e.name, show(&e.capabilities), refined);
    }
    println!("  {:<width$}  {}", "total", show(&fp.total));
    // The build axis (only when the rune ships a `build` step). Runtime authority
    // is enforced by the type system; the build footprint is the supply-chain
    // signal — what a rune's build step is allowed to do, outside the consumer's
    // type-checked call graph.
    if !fp.build.is_empty() {
        println!("Build-time footprint of {path}:");
        println!("  {:<width$}  {}", "build", show(&fp.build));
    }
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
    if !old.build.is_empty() || !new.build.is_empty() {
        println!("  build old:  {}", capabilities::show_caps(&old.build));
        println!("  build new:  {}", capabilities::show_caps(&new.build));
        println!("  build +:    {}", capabilities::show_caps(&d.build_added));
        println!("  build -:    {}", capabilities::show_caps(&d.build_removed));
    }
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
    let mut flagged = false;
    if d.build_widened() {
        // The high-signal supply-chain event: build-time execution is outside the
        // consumer's type-checked call graph, so a new build cap is the thing the
        // gate must catch.
        println!(
            "BUILD WIDENING: the newer version's build step demands new build-time authority ({}). \
             It cannot run until you grant it (`--allow-build-cap` + a `[build.grants]` entry).",
            capabilities::show_caps(&d.build_added)
        );
        flagged = true;
    }
    if !d.added.is_empty() {
        println!(
            "WIDENING: the newer version demands new host authority ({}). Review before trusting.",
            capabilities::show_caps(&d.added)
        );
        flagged = true;
    }
    if !flagged {
        if !d.refinements_dropped.is_empty() {
            // Same authority on both axes, but a brand was dropped — a confined
            // capability loosened to its bare form. Not a widening, but an intent
            // change worth surfacing.
            println!(
                "OK on authority, but a refinement was dropped ({}): a confined capability loosened to its bare form. Worth a look.",
                join(&d.refinements_dropped)
            );
        } else {
            println!("OK: no widening — the newer version demands no new authority on either axis.");
        }
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

/// Compile a whole actor PROGRAM and run it on the actor system: `main`
/// executes in a driver VM, each `spawn` instantiates that actor's own WASM
/// VM through a host import, and `send` routes across the isolated VMs.
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
    let (driver, actors, sigs, specs) = match codegen::compile_system(&module) {
        Ok(v) => v,
        Err(e) => {
            println!("{e}");
            return;
        }
    };
    match actor_system::System::run_program(&driver, &actors, sigs, specs, &actor_system::dev_caps()) {
        Ok(output) => {
            for line in output {
                println!("{line}");
            }
        }
        Err(e) => println!("run failed: {e}"),
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

    /// The parameter conventions (`var`/`let`/`own` + `move`) behave identically
    /// on both the interpreter and WASM backends — value semantics are
    /// preserved regardless of which knob the author reaches for. `var` writes
    /// back, `let` borrows (read-only), `own` consumes, a bare param is owned, and
    /// `move x` transfers ownership.
    #[test]
    fn conventions_backends_agree() {
        let src = "fn bump(var n: Int):\n    n = n + 1\n\nfn total(let xs: List(Int)) -> Int:\n    var s = 0\n    for x in xs:\n        s = s + x\n    s\n\nfn drain(own xs: List(Int)) -> Int:\n    list.length(xs)\n\nfn doubled(xs: List(Int)) -> Int:\n    list.at(xs, 0) * 2\n\nfn main(console: Console):\n    var c = 0\n    bump(c)\n    bump(c)\n    print(console, to_string(c))\n    let nums = [10, 20, 30]\n    print(console, to_string(total(nums)))\n    print(console, to_string(doubled(nums)))\n    print(console, to_string(list.length(nums)))\n    let g = [1, 2, 3, 4]\n    print(console, to_string(drain(move g)))\n";
        let expected = ["2", "60", "20", "3", "4"];
        assert_eq!(interpreter::run(src).expect("interp"), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// Misusing the ownership conventions is rejected up front by the type checker
    /// (so the same program fails on every backend, never just native): using a
    /// value after it was consumed by `own`, or after `move`. A bare `let` borrow
    /// imposes no such restriction.
    #[test]
    fn conventions_reuse_after_move_rejected() {
        // Reuse after an `own` (sink) parameter consumes it.
        let after_own = "fn drain(own xs: List(Int)) -> Int:\n    list.length(xs)\nfn main(c: Console):\n    let d = [1, 2, 3]\n    print(c, to_string(drain(d)))\n    print(c, to_string(list.length(d)))\n";
        let e1 = typeck::check_str(after_own).expect_err("reuse after own should fail");
        assert!(e1.to_string().contains("after it was moved"), "got: {e1:?}");
        // Reuse after an explicit `move`.
        let after_move = "fn drain(own xs: List(Int)) -> Int:\n    list.length(xs)\nfn main(c: Console):\n    let d = [1, 2, 3]\n    print(c, to_string(drain(move d)))\n    print(c, to_string(list.length(d)))\n";
        assert!(
            typeck::check_str(after_move).is_err(),
            "reuse after move should fail"
        );
        // A `let` borrow does NOT consume — reuse is fine.
        let after_borrow = "fn peek(let xs: List(Int)) -> Int:\n    list.length(xs)\nfn main(c: Console):\n    let d = [1, 2, 3]\n    print(c, to_string(peek(d)))\n    print(c, to_string(list.length(d)))\n";
        assert!(typeck::check_str(after_borrow).is_ok(), "borrow reuse should be fine");
    }

    /// The full conventions showcase (examples/conventions.witchy) — `var`/`let`/
    /// `own`/`move` across a function, a method (`let self`), an actor (`var`
    /// state, `own` payload), and local bindings — runs identically on the
    /// interpreter and WASM backends.
    #[test]
    fn conventions_showcase_runs() {
        let expected = "count: 2\nsum: 10\ndoubled first: 2\nnums still here, length: 4\nbag total: 60\ndrained length: 3\nrunning sum: 300\nrunning sum: 306\n";
        let (linked, _) = crate::link_file("examples/conventions.witchy").expect("link");
        let interp =
            interpreter::run_module(linked, ".", Vec::new()).expect("interp run").join("\n");
        assert_eq!(format!("{interp}\n"), expected, "interp showcase");
    }

    /// `let` borrows extend past `List` to the other heap types: a `String`
    /// parameter (the recursive-parser shape that motivated this — char ops on a
    /// borrowed string, no clone) and a `Dict`. Native emits `&String` / `&WMap`
    /// and the output matches every backend.
    #[test]
    fn convention_string_and_dict_borrow() {
        let strs = "fn first_char(let s: String) -> String:\n    if string.char_count(s) > 0:\n        string.substring(s, 0, 1)\n    else:\n        \"\"\nfn main(c: Console):\n    let txt = \"héllo\"\n    print(c, first_char(txt))\n    print(c, to_string(string.char_count(txt)))\n";
        assert_eq!(interpreter::run(strs).expect("interp str"), ["h", "5"]);
        assert_eq!(run_linked_on_wasm(&[("main", strs)], "main"), ["h", "5"], "wasm str");

        let dict = "fn lookup(let d: Dict(String, Int)) -> Int:\n    dict.get_or(d, \"a\", -1)\nfn main(c: Console):\n    var m = dict.new()\n    m = dict.insert(m, \"a\", 42)\n    print(c, to_string(lookup(m)))\n    print(c, to_string(dict.size(m)))\n";
        assert_eq!(interpreter::run(dict).expect("interp dict"), ["42", "1"]);
        assert_eq!(run_linked_on_wasm(&[("main", dict)], "main"), ["42", "1"], "wasm dict");
    }

    /// `move` works in every value position (let value, list element, call
    /// argument), forcing a move; the moved binding can't be reused (rejected by
    /// the type checker, uniformly).
    #[test]
    fn convention_move_value_positions() {
        let prog = "fn main(console: Console):\n    let a = [1, 2, 3]\n    let b = move a\n    print(console, to_string(list.length(b)))\n";
        assert_eq!(interpreter::run(prog).expect("interp"), ["3"]);
        assert_eq!(run_linked_on_wasm(&[("main", prog)], "main"), ["3"], "wasm");
        // Reuse after move is rejected everywhere.
        let reuse = "fn main(console: Console):\n    let a = [1, 2, 3]\n    let b = move a\n    print(console, to_string(list.length(b) + list.length(a)))\n";
        assert!(typeck::check_str(reuse).is_err(), "reuse after move must fail");
    }

    /// A borrow can't escape: returning a `let` parameter transpiles, but Rust's
    /// borrow checker rejects it at compile time (the opt-in contract — drop `let`
    /// or use `own`). A non-escaping borrow compiles fine.
    #[test]
    fn convention_borrow_cannot_escape() {
        // Returning a `let` parameter escapes the borrow — a TYPE error on
        // every backend (the rule moved from the removed native backend's
        // borrow checker into typeck).
        let escapes = "fn id(let xs: List(Int)) -> List(Int):\n    xs\nfn main(c: Console):\n    print(c, to_string(list.length(id([1, 2, 3]))))\n";
        let err = typeck::check_str(escapes).expect_err("escaping borrow must be rejected");
        assert!(err.to_string().contains("cannot be returned"), "{err}");
        // Reading it (no escape) is fine.
        let reads = "fn count(let xs: List(Int)) -> Int:\n    list.length(xs)\nfn main(c: Console):\n    print(c, to_string(count([1, 2, 3])))\n";
        assert!(typeck::check_str(reads).is_ok(), "a read-only borrow should check");
    }

    /// Conventions apply to a method's receiver too: `let self` borrows it
    /// (read-only), and `own self` consumes it (the value can't be used after the
    /// call). Both run identically on interpreter and native.
    #[test]
    fn convention_method_receivers() {
        // `let self` — borrow the receiver, return a fresh value (functional style).
        let borrow_self = "type Counter:\n    Counter(Int)\nimpl Counter:\n    fn incremented(let self) -> Counter:\n        match self:\n            Counter(n) -> Counter(n + 1)\nfn main(c: Console):\n    let a = Counter(5)\n    match a.incremented():\n        Counter(n) -> print(c, to_string(n))\n";
        // `own self` — consume the receiver.
        let own_self = "import list\ntype Buffer:\n    Buffer(List(Int))\nimpl Buffer:\n    fn drain(own self) -> Int:\n        match self:\n            Buffer(xs) -> list.sum(xs)\nfn main(c: Console):\n    let buf = Buffer([1, 2, 3])\n    print(c, to_string(buf.drain()))\n";
        for (tag, src) in [("let_self", borrow_self), ("own_self", own_self)] {
            assert_eq!(link_run(src), vec!["6"], "{tag} interp");
            assert_eq!(wasm_run(src), vec!["6"], "{tag} wasm");
        }
    }

    /// A borrow can be forwarded BOTH ways: to another borrow parameter it passes
    /// straight through (`&T` -> `&T`, no copy), and to an owned parameter it is
    /// deref-cloned (you can't move out of a borrow). Same result on every backend.
    #[test]
    fn convention_borrow_forwarding() {
        let src = "fn owned_first(xs: List(Int)) -> Int:\n    list.at(xs, 0) * 2\n\nfn borrowed_len(let ys: List(Int)) -> Int:\n    list.length(ys)\n\nfn report(let xs: List(Int)) -> Int:\n    borrowed_len(xs) + owned_first(xs)\n\nfn main(c: Console):\n    let data = [5, 6, 7]\n    print(c, to_string(report(data)))\n    print(c, to_string(list.length(data)))\n";
        assert_eq!(interpreter::run(src).expect("interp"), ["13", "3"]);
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), ["13", "3"], "wasm");
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
            "fn fib(n: Int) -> Int:\n    if n < 2:\n        n\n    else:\n        fib(n - 1) + fib(n - 2)\nfn main(console: Console):\n    print(console, to_string(fib(10)))\n",
        )
        .expect("write temp source");
        let wat = crate::emit_wat_file(path.to_str().unwrap()).expect("emit-wat");
        let _ = std::fs::remove_file(&path);
        assert!(wat.starts_with("(module"), "expected a wasm module, got: {}", &wat[..wat.len().min(40)]);
        // The fib function is emitted, module-qualified by the file stem.
        assert!(wat.contains(".fib (param $n i64)"), "expected the fib function in the WAT");
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
    print(console, to_string(a))
    var b = 0
    for i in 1..=5:
        b = b + i
    print(console, to_string(b))
    var c = 0
    for i in 0..100:
        if i == 10:
            break
        c = c + i
    print(console, to_string(c))
    var d = 0
    for i in 0..10:
        if i % 2 == 0:
            continue
        d = d + i
    print(console, to_string(d))
    var e = 0
    for i in 5..5:
        e = e + 1
    for i in 5..2:
        e = e + 1
    print(console, to_string(e))
    var f = 0
    for i in 0..3:
        for j in 0..3:
            f = f + i * j
    print(console, to_string(f))
    var g = 0
    for i in 0..100000:
        g = g + 1
    print(console, to_string(g))
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
                    "fn main(console: Console):\n    var s = 0\n    for i in {lo}{op}{hi}:\n        s = s + i\n    print(console, to_string(s))\n"
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
                    "fn main(console: Console):\n    var s = 0\n    for i in {lo}..{hi}:\n        if i % 2 != 0:\n            continue\n        s = s + i\n    print(console, to_string(s))\n"
                );
                let reference: i64 = (lo..hi).filter(|x| x % 2 == 0).sum();
                let want = vec![reference.to_string()];
                prop_assert_eq!(interp(&src), want.clone());
                prop_assert_eq!(run_on_wasm(&src), want);
            }
        }
    }

    /// Property tests over the standard library: invariants that must hold for
    /// *any* input — encode/decode round-trips, calendar inverses, semver
    /// rendering — checked by generating the input, running it through the witchy
    /// stdlib, and comparing to a Rust reference. These catch edge cases (empty
    /// strings, embedded quotes/newlines, negative timestamps) unit tests miss.
    mod stdlib_properties {
        use super::link_run;
        use proptest::prelude::*;

        /// Escape a Rust string into the body of a witchy `"..."` literal.
        fn esc(s: &str) -> String {
            let mut out = String::new();
            for c in s.chars() {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '"' => out.push_str("\\\""),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    _ => out.push(c),
                }
            }
            out
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]

            /// `encoding.hex_encode` equals the byte-wise lowercase hex reference.
            #[test]
            fn hex_encode_matches_reference(s in "[ -#%-z|~]{0,40}") {
                let src = format!(
                    "import encoding\nfn main(console: Console):\n    print(console, encoding.hex_encode(\"{}\"))\n",
                    esc(&s)
                );
                let reference: String = s.bytes().map(|b| format!("{b:02x}")).collect();
                prop_assert_eq!(link_run(&src), vec![reference]);
            }

            /// base64 decode is the inverse of encode, for any printable ASCII.
            #[test]
            fn base64_roundtrips(s in "[ -#%-z|~]{0,48}") {
                let src = format!(
                    "import encoding\nfn main(console: Console):\n    let s = \"{}\"\n    print(console, yn(encoding.base64_decode(encoding.base64_encode(s)) == s))\n\nfn yn(b: Bool) -> String:\n    if b: \"y\" else: \"n\"\n",
                    esc(&s)
                );
                prop_assert_eq!(link_run(&src), vec!["y".to_string()]);
            }

            /// hex decode is the inverse of encode.
            #[test]
            fn hex_roundtrips(s in "[ -#%-z|~]{0,48}") {
                let src = format!(
                    "import encoding\nfn main(console: Console):\n    let s = \"{}\"\n    print(console, yn(encoding.hex_decode(encoding.hex_encode(s)) == s))\n\nfn yn(b: Bool) -> String:\n    if b: \"y\" else: \"n\"\n",
                    esc(&s)
                );
                prop_assert_eq!(link_run(&src), vec!["y".to_string()]);
            }

            /// `time.to_unix` is the exact inverse of `time.from_unix`, across the
            /// CE range and negative (pre-1970) timestamps.
            #[test]
            fn time_unix_roundtrips(n in -62135596800i64..=253402300799i64) {
                let src = format!(
                    "import time\nfn main(console: Console):\n    print(console, yn(time.to_unix(time.from_unix({n})) == {n}))\n\nfn yn(b: Bool) -> String:\n    if b: \"y\" else: \"n\"\n"
                );
                prop_assert_eq!(link_run(&src), vec!["y".to_string()]);
            }

            /// A single CSV field round-trips through encode/parse — including
            /// embedded commas, quotes, and newlines (the cases that need quoting).
            #[test]
            fn csv_field_roundtrips(s in "[a-zA-Z0-9 ,\"\n]{0,24}") {
                let src = format!(
                    "import csv\nfn main(console: Console):\n    let s = \"{}\"\n    let rows = csv.parse(csv.encode([[s]]))\n    print(console, yn(list.length(rows) == 1 && list.length(list.at(rows, 0)) == 1 && list.at(list.at(rows, 0), 0) == s))\n\nfn yn(b: Bool) -> String:\n    if b: \"y\" else: \"n\"\n",
                    esc(&s)
                );
                prop_assert_eq!(link_run(&src), vec!["y".to_string()]);
            }

            /// `semver.to_string` after `parse` reproduces the canonical version.
            #[test]
            fn semver_roundtrips(a in 0i64..2000, b in 0i64..2000, c in 0i64..2000) {
                let v = format!("{a}.{b}.{c}");
                let src = format!(
                    "import semver\nfn main(console: Console):\n    match semver.parse(\"{v}\"):\n        Ok(x) -> print(console, semver.format(x))\n        Err(e) -> print(console, \"err\")\n"
                );
                prop_assert_eq!(link_run(&src), vec![v]);
            }

            /// `path.normalize` is idempotent — normalizing an already-normal path
            /// changes nothing — over arbitrary `.`/`..`/segment soup.
            #[test]
            fn path_normalize_is_idempotent(p in "[a-c./]{0,24}") {
                let src = format!(
                    "import path\nfn main(console: Console):\n    let once = path.normalize(\"{}\")\n    print(console, yn(path.normalize(once) == once))\n\nfn yn(b: Bool) -> String:\n    if b: \"y\" else: \"n\"\n",
                    esc(&p)
                );
                prop_assert_eq!(link_run(&src), vec!["y".to_string()]);
            }

            /// Run-length decode is the inverse of encode (the `examples/rle`
            /// algorithm, exercising string.to_chars/repeat + ascii.is_digit/
            /// to_digit). Restricted to digit-free input: the count-prefix format
            /// is only unambiguous when the data carries no digits, so this both
            /// asserts the round-trip and documents that boundary.
            #[test]
            fn rle_round_trips_over_digit_free_text(s in "[a-zA-Z ]{0,40}") {
                let src = format!(
                    "import string\nimport ascii\n\nfn encode(t: String) -> String:\n    let cs = string.chars(t)\n    let n = list.length(cs)\n    var out = \"\"\n    var i = 0\n    while i < n:\n        let c = list.at(cs, i)\n        var k = 0\n        while i < n && list.at(cs, i) == c:\n            k = k + 1\n            i = i + 1\n        out = out <> to_string(k) <> c\n    out\n\nfn decode(e: String) -> String:\n    let cs = string.chars(e)\n    let n = list.length(cs)\n    var out = \"\"\n    var i = 0\n    while i < n:\n        var k = 0\n        while i < n && ascii.is_digit(list.at(cs, i)):\n            k = k * 10 + ascii.to_digit(list.at(cs, i))\n            i = i + 1\n        if i < n:\n            out = out <> string.repeat(list.at(cs, i), k)\n            i = i + 1\n    out\n\nfn yn(b: Bool) -> String:\n    if b: \"y\" else: \"n\"\n\nfn main(console: Console):\n    let s = \"{}\"\n    print(console, yn(decode(encode(s)) == s))\n",
                    esc(&s)
                );
                prop_assert_eq!(link_run(&src), vec!["y".to_string()]);
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

    fn wasm_run(src: &str) -> Vec<String> {
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::linker::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let wat = codegen::compile_module(&linked).expect("compile");
        crate::run_wat_capture(&wat).expect("wasm run")
    }

    /// `wasm_run` that also reads the exported `__witchy_reowns` counter —
    /// the timing-free proof of whether accumulation ran in place (O(1)
    /// re-owns) or fell to the copying path (O(n) re-owns).
    fn wasm_run_reowns(src: &str) -> (Vec<String>, i64) {
        use crate::runtime::{Capabilities, Runtime};
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::linker::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let wat = codegen::compile_module(&linked).expect("compile");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                wat.as_bytes(),
                Capabilities { print: true, print_int: true, quiet: true, ..Default::default() },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn");
        actor.run().expect("run");
        let reowns = actor.reowns().unwrap_or(0);
        (actor.output(), reowns)
    }

    /// THE UNIQUENESS ANALYSIS, observable: an alias taken BEFORE the loop
    /// zeroes the ownership token once — the first push re-owns (one copy)
    /// and everything after runs in place. The old syntactic whitelist
    /// disqualified the variable outright (O(n²), memory-cap trap at this
    /// size). The alias still sees its snapshot.
    #[test]
    fn analysis_alias_before_loop_stays_linear() {
        let src = "fn main(console: Console):\n    var xs = [1, 2, 3]\n    let snapshot = xs\n    var i = 0\n    while i < 50000:\n        xs = list.push(xs, i)\n        i = i + 1\n    print(console, to_string(snapshot))\n    print(console, to_string(list.length(xs)))\n";
        let want = vec!["[1, 2, 3]".to_string(), "50003".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        let (out, reowns) = wasm_run_reowns(src);
        assert_eq!(out, want, "wasm");
        assert!(reowns <= 2, "expected O(1) re-owns, got {reowns}");
    }

    /// An alias RE-TAKEN inside the loop forces the copying path each
    /// iteration (the kill re-zeroes the token) — correct, O(n) re-owns, and
    /// exactly what the cliff diagnostic exists to flag.
    #[test]
    fn analysis_alias_inside_loop_reowns_per_iteration() {
        let src = "fn main(console: Console):\n    var ys = []\n    var last = [9]\n    var j = 0\n    while j < 200:\n        ys = list.push(ys, j)\n        last = ys\n        j = j + 1\n    print(console, to_string(list.length(last)))\n";
        let want = vec!["200".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        let (out, reowns) = wasm_run_reowns(src);
        assert_eq!(out, want, "wasm");
        assert!(reowns >= 150, "every iteration must re-own, got {reowns}");
    }

    /// FUNCTION SUMMARIES: a read-only helper called in the hot loop no
    /// longer kills the token (the bottom-up pass proves its parameter never
    /// aliases out). Under the whitelist this was an instant disqualification.
    #[test]
    fn analysis_readonly_call_keeps_loop_linear() {
        let src = "fn peek(xs: List(Int)) -> Int:\n    list.length(xs)\n\nfn main(console: Console):\n    var ws = []\n    var m = 0\n    var probe = 0\n    while m < 3000:\n        ws = list.push(ws, m)\n        probe = peek(ws)\n        m = m + 1\n    print(console, to_string(probe))\n";
        let want = vec!["3000".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        let (out, reowns) = wasm_run_reowns(src);
        assert_eq!(out, want, "wasm");
        assert!(reowns <= 2, "the summary must keep the loop in place, got {reowns}");
    }

    /// A function that RETURNS its parameter (may_alias_out) still kills:
    /// the bound result whole-aliases the buffer, so the next push copies —
    /// and the alias keeps its snapshot.
    #[test]
    fn analysis_alias_returning_call_still_kills() {
        let src = "fn same(xs: List(Int)) -> List(Int):\n    xs\n\nfn main(console: Console):\n    var xs = [1]\n    var i = 0\n    while i < 100:\n        xs = list.push(xs, i)\n        i = i + 1\n    let held = same(xs)\n    xs = list.push(xs, 999)\n    print(console, to_string(list.length(held)))\n    print(console, to_string(list.length(xs)))\n";
        let want = vec!["101".to_string(), "102".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        let (out, _) = wasm_run_reowns(src);
        assert_eq!(out, want, "wasm");
    }

    /// DIRTY SITES: a self-assign whose RHS embeds the variable (`s = s <> s`,
    /// a pushed snapshot stored into a dict) runs through the copying path
    /// and stays value-semantic on both backends.
    #[test]
    fn analysis_dirty_shapes_stay_value_semantic() {
        let src = "fn main(console: Console):\n    var s = \"ab\"\n    var k = 0\n    while k < 5:\n        s = s <> s\n        k = k + 1\n    print(console, to_string(string.length(s)))\n    var d = dict.new()\n    var zs = [1]\n    d = dict.insert(d, \"snap\", zs)\n    zs = list.push(zs, 2)\n    print(console, to_string(list.length(dict.get_or(d, \"snap\", []))))\n    print(console, to_string(list.length(zs)))\n";
        let want: Vec<String> = ["64", "1", "2"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// A lambda body is its own analysis unit: an accumulator inside one gets
    /// its own ownership token (this used to emit an undeclared `__cap`
    /// local — a loud compile failure).
    #[test]
    fn analysis_lambda_accumulator_compiles() {
        let src = "fn main(console: Console):\n    let build = fn(n: Int):\n        var acc = [0]\n        var t = 0\n        while t < n:\n            acc = list.push(acc, t)\n            t = t + 1\n        list.length(acc)\n    print(console, to_string(build(1000)))\n";
        let want = vec!["1001".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// `derive(Show, Eq, Ord)`: compiler-generated impls, byte-identical in
    /// behavior to handwritten ones on both backends, additive-only (the
    /// expansion appends impls before checking; footprint analysis covers
    /// the expanded program).
    #[test]
    fn derive_show_eq_ord_generates_working_impls() {
        let src = "import show\nimport ord\nimport list\n\ntype Point derive(Show, Eq, Ord):\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let a = Point(1, 2)\n    let b = Point(1, 3)\n    say(console, a)\n    print(console, \"${eq(a, Point(1, 2))} ${eq(a, b)}\")\n    print(console, \"${less(a, b)} ${less(b, a)}\")\n    print(console, \"${list.contains([a, b], Point(1, 3))}\")\n";
        let want: Vec<String> = ["Point(1, 2)", "true false", "true false", "true"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
        // Unknown derives are loud.
        let bad = "type T derive(Serialize):\n    n: Int\n\nfn main(console: Console):\n    print(console, \"x\")\n";
        let module = parser::parse_module(bad).expect("parse");
        let err = crate::records::lower(module).expect_err("unknown derive must be rejected");
        assert!(err.contains("unknown derive"), "got: {err}");
    }

    /// `comptime:` — compile-time item generation: zero capabilities
    /// reachable (deterministic by construction), `emit(line)` as the
    /// channel, output parsed as ADDITIVE items before checking — so the
    /// generated functions exist on both backends and in the footprint.
    #[test]
    fn comptime_blocks_generate_items_additively() {
        let src = "comptime:\n    var i = 0\n    while i < 3:\n        emit(\"pub fn lucky_${i}() -> Int:\")\n        emit(\"    ${i * 7}\")\n        emit(\"\")\n        i = i + 1\n\nfn main(console: Console):\n    print(console, \"${lucky_0()} ${lucky_1()} ${lucky_2()}\")\n";
        let want = vec!["0 7 14".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
        // Emitted garbage is a loud error carrying the emitted source.
        let bad = "comptime:\n    emit(\"fn (((\")\n\nfn main(console: Console):\n    print(console, \"x\")\n";
        let module = parser::parse_module(bad).expect("parse");
        let err = crate::linker::link(vec![("main".into(), module)], "main")
            .expect_err("bad emission must be loud");
        assert!(err.to_string().contains("does not parse"), "got: {err}");
    }

    /// Tuple patterns in `for` (the learning log's F4): `for (k, v) in
    /// dict.pairs(d):` destructures per element, round-trips through fmt,
    /// and agrees on both backends.
    #[test]
    fn for_tuple_patterns_destructure() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    d = dict.insert(d, \"a\", 1)\n    d = dict.insert(d, \"b\", 2)\n    for (k, v) in dict.pairs(d):\n        print(console, \"${k}=${v}\")\n";
        let want: Vec<String> = ["a=1", "b=2"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
        assert_eq!(
            crate::format::reformat(src).as_deref(),
            Some(src),
            "the sugar must round-trip through fmt"
        );
    }

    /// VALUE EQUALITY, ALWAYS (the learning log's F15): dict lookups with
    /// RUNTIME-BUILT keys (trim/split/concat-sourced) — the case literal-key
    /// tests pass vacuously through interning. dict.get/has must find them
    /// by CONTENT on both backends; the compiled tier used to silently
    /// pointer-compare and return None.
    #[test]
    fn runtime_built_dict_keys_compare_by_content() {
        let src = "import dict\nimport string\n\nfn main(console: Console):\n    var d = dict.new()\n    d = dict.insert(d, string.trim(\"  host  \"), \"localhost\")\n    let parts = string.split(\"port=8080\", \"=\")\n    d = dict.insert(d, list.at(parts, 0), list.at(parts, 1))\n    d = dict.insert(d, \"lit\" <> \"eral\", \"joined\")\n    match dict.get(d, \"host\"):\n        Some(v) -> print(console, \"host=\" <> v)\n        None -> print(console, \"host MISSING\")\n    match dict.get(d, \"port\"):\n        Some(v) -> print(console, \"port=\" <> v)\n        None -> print(console, \"port MISSING\")\n    print(console, \"${dict.has(d, \"literal\")}\")\n    print(console, \"${dict.size(d)}\")\n";
        let want: Vec<String> = ["host=localhost", "port=8080", "true", "3"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// Generic stdlib functions over USER RECORD types compare by content:
    /// typed lowering resolves the type argument (confirmed via the table),
    /// the specialization's `==` becomes structural. Previously the generic
    /// fallback pointer-compared (or, post-hotfix, refused to compile).
    #[test]
    fn generic_equality_on_records_is_structural() {
        let src = "import list\n\ntype Point:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let pts = [Point(1, 2), Point(3, 4)]\n    let probe = Point(1 + 2, 4)\n    print(console, \"${list.contains(pts, probe)}\")\n    print(console, \"${list.index_of(pts, Point(1, 2))}\")\n";
        let want: Vec<String> = ["true", "0"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// THE F11 FAMILY (learning log): interpolating values whose type only
    /// typed lowering knows — an ADT String payload and a generic-combinator
    /// return — renders identically on both backends.
    #[test]
    fn interpolation_of_mono_typed_values_agrees() {
        let src = "import iter\n\ntype Msg:\n    Text(String)\n    Silence\n\nfn main(console: Console):\n    match Text(\"hi\"):\n        Text(s) -> print(console, \"got: ${s}\")\n        Silence -> print(console, \"none\")\n    let collected = iter.collect(iter.take(iter.range(1, 100), 3))\n    print(console, \"collected: ${collected}\")\n";
        let want: Vec<String> = ["got: hi", "collected: [1, 2, 3]"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// `say` covers every scalar out of the box (Duration in its HUMAN form
    /// — the custom rendering `Show` exists for), and a missing impl is a
    /// clean check-time error naming the trait and type, not a post-lowering
    /// "unknown function" artifact.
    #[test]
    fn show_scalars_and_missing_impl_diagnostic() {
        let src = "import show\n\nfn main(console: Console):\n    say(console, 42)\n    say(console, 3.5)\n    say(console, 90s)\n    say(console, true)\n";
        let want: Vec<String> =
            ["42", "3.5", "1m30s", "true"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
        let missing = "import show\n\ntype Blob:\n    n: Int\n\nfn main(console: Console):\n    say(console, Blob(1))\n";
        let module = parser::parse_module(missing).expect("parse");
        let linked =
            crate::linker::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked).expect_err("missing impl must be rejected");
        assert!(
            err.to_string().contains("`Blob` does not implement `Show`"),
            "want a clean trait error, got: {err}"
        );
    }

    /// The formatter ROUND-TRIPS string interpolation (the lexer desugars it
    /// to a `<>` chain; `interpolation_sugar` prints it back), and
    /// canonicalizes a hand-written chain of the exact desugared shape into
    /// the idiom.
    #[test]
    fn fmt_round_trips_interpolation() {
        let src = "fn main(console: Console):\n    let n = 3\n    print(console, \"n is ${n}, doubled ${n * 2}\")\n    print(console, \"cost: \\$${n}\")\n";
        assert_eq!(crate::format::reformat(src).as_deref(), Some(src), "interpolation must round-trip");
        let chain = "fn main(console: Console):\n    let n = 3\n    print(console, \"n is \" <> to_string(n) <> \"\")\n";
        let want = "fn main(console: Console):\n    let n = 3\n    print(console, \"n is ${n}\")\n";
        assert_eq!(
            crate::format::reformat(chain).as_deref(),
            Some(want),
            "the canonical chain shape prints as interpolation"
        );
    }

    /// THE OWN-ABI: `xs = grow(move xs, i)` is a linear pipeline — the
    /// ownership token crosses the call in both directions (an extra cap
    /// param and result), so a cross-function builder stays O(n). Without
    /// the transfer each call re-owned by copy: O(n²) — the reowns counter
    /// (not timing) is the proof. (The interpreter leg stays small: it
    /// clones at every call by design.)
    #[test]
    fn analysis_own_abi_pipelines_in_place() {
        let src = "fn grow(own xs: List(Int), n: Int) -> List(Int):\n    xs = list.push(xs, n)\n    xs\n\nfn main(console: Console):\n    var xs = [0]\n    var i = 0\n    while i < 3000:\n        xs = grow(move xs, i)\n        i = i + 1\n    print(console, to_string(list.length(xs)))\n    print(console, to_string(list.at(xs, 3000)))\n";
        let want = vec!["3001".to_string(), "2999".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        let (out, reowns) = wasm_run_reowns(src);
        assert_eq!(out, want, "wasm");
        assert!(reowns <= 2, "the token must survive the calls, got {reowns} re-owns");
    }

    /// An own-ABI callee that returns its parameter only on SOME paths: the
    /// other paths return a zero token (the caller re-owns later) — always
    /// correct, never corrupting.
    #[test]
    fn analysis_own_abi_partial_return_paths_are_sound() {
        let src = "fn cap_at(own xs: List(Int), n: Int) -> List(Int):\n    if list.length(xs) >= n:\n        []\n    else:\n        xs = list.push(xs, n)\n        xs\n\nfn main(console: Console):\n    var xs = [0]\n    var i = 0\n    while i < 50:\n        xs = cap_at(move xs, i)\n        i = i + 1\n    print(console, to_string(xs))\n";
        let interp = link_run(src);
        assert_eq!(wasm_run(src), interp, "wasm must agree on the mixed paths");
    }

    /// THE FORCED-COPY DIFFERENTIAL: `WITCHY_NO_INPLACE` compiles with the
    /// in-place machinery off (the copying paths ARE the semantics). Outputs
    /// must be identical — any divergence is an analysis soundness bug.
    #[test]
    fn forced_copy_mode_is_differential() {
        let src = "fn tag(let prefix: String, n: Int) -> String:\n    prefix <> to_string(n)\n\nfn main(console: Console):\n    var xs = []\n    let alias = xs\n    var s = \"\"\n    var d = dict.new()\n    var i = 0\n    while i < 800:\n        xs = list.push(xs, i)\n        s = s <> tag(\"x\", i)\n        d = dict.update(d, i % 7, 0, fn(n: Int): n + 1)\n        i = i + 1\n    print(console, to_string(list.length(xs)))\n    print(console, to_string(list.length(alias)))\n    print(console, to_string(string.length(s)))\n    print(console, to_string(dict.get_or(d, 3, 0)))\n";
        let optimized = wasm_run(src);
        codegen::set_force_copy_for_tests(Some(true));
        let forced = wasm_run(src);
        codegen::set_force_copy_for_tests(None);
        assert_eq!(optimized, forced, "forced-copy output must match the optimized build");
        assert_eq!(link_run(src), optimized, "and both must match the interpreter");
    }

    /// `crypto.rune_hash` produces the same store hash (`src/pm/store.rs`
    /// format) on both backends — the host walks the guest's string lists.
    #[test]
    fn crypto_rune_hash_runs_in_the_wasm_backend() {
        let prog = "import crypto\nfn main(console: Console):\n    print(console, crypto.rune_hash([\"a.witchy\", \"b.witchy\"], [\"fn one\", \"fn two\"]))\n";
        let out = wasm_run(prog);
        assert_eq!(out, link_run(prog));
        assert!(out[0].starts_with("sha256:") && out[0].len() == 71, "{out:?}");
    }

    /// `compiler.footprint` runs in the WASM backend (staged-JSON host bridge)
    /// and agrees byte-for-byte with the interpreter — a self-hosted package
    /// manager can compute footprints from inside the sandbox.
    #[test]
    fn compiler_footprint_runs_in_the_wasm_backend() {
        let prog = "import compiler\nfn main(console: Console):\n    print(console, compiler.footprint(\"pub fn read_all(d: Dir[Read]) -> String:\\n    read(d, \\\"x\\\")\\n\"))\n";
        let out = wasm_run(prog);
        assert_eq!(out, link_run(prog));
        assert!(out[0].contains("Dir[Read]"), "{out:?}");
    }

    /// `compiler.diff` runs in the WASM backend and flags widening exactly as
    /// the interpreter does.
    #[test]
    fn compiler_diff_runs_in_the_wasm_backend() {
        let prog = "import compiler\nfn main(console: Console):\n    let old = \"pub fn pure(x: Int) -> Int:\\n    x\\n\"\n    let new = \"pub fn pure(x: Int, d: Dir) -> Int:\\n    x\\n\"\n    print(console, compiler.diff(old, new))\n";
        let out = wasm_run(prog);
        assert_eq!(out, link_run(prog));
        assert!(out[0].contains("\"widened\":true"), "{out:?}");
    }

    /// The full `std/http` client runs in the WASM backend: a real GET against
    /// a local server returns the same status and body on both backends.
    #[test]
    fn std_http_client_runs_in_the_wasm_backend() {
        use crate::runtime::{Capabilities, Runtime};
        use std::io::{BufRead, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = server.local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        // One request per backend run: consume the request head, reply 200.
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let (stream, _) = server.accept().expect("accept");
                let mut reader = std::io::BufReader::new(stream);
                let mut line = String::new();
                loop {
                    line.clear();
                    reader.read_line(&mut line).expect("read");
                    if line == "\r\n" || line == "\n" || line.is_empty() {
                        break;
                    }
                }
                reader
                    .get_mut()
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello")
                    .expect("write");
            }
        });

        let src = format!(
            "import http\nfn main(console: Console, net: Net):\n    let res = http.get(net, \"127.0.0.1\", {port}, \"/greet\")\n    print(console, f\"{{http.status(res)}} {{http.body(res)}}\")\n"
        );
        let want = vec!["200 hello".to_string()];
        let module = parser::parse_module(&src).expect("parse");
        let linked = crate::linker::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", vec![addr.clone()]).expect("interp"),
            want,
            "interpreter"
        );
        let wat = codegen::compile_module(&linked).expect("compile");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                wat.as_bytes(),
                Capabilities {
                    print: true,
                    quiet: true,
                    net_allow: Some(vec![addr]),
                    net_connect: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");
        handle.join().expect("server thread");
    }

    /// Signing round-trips entirely in witchy: a host-granted `Secret`
    /// capability signs a message (`crypto.sign`), and `crypto.ed25519_verify`
    /// against the key's public half (`crypto.public_key`) accepts it. Without a
    /// granted key, a `Secret` parameter is refused, and the capability
    /// surfaces in the footprint.
    #[test]
    fn crypto_signing_round_trips_in_witchy() {
        let src = "import crypto\nfn main(console: Console, signer: Secret):\n    let msg = \"sign me\"\n    let sig = crypto.sign(signer, msg)\n    print(console, if crypto.ed25519_verify(crypto.public_key(signer), msg, sig): \"verified\" else: \"FAILED\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::linker::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let out = interpreter::run_module_signed(linked, ".", Vec::new(), Vec::new(), Some([7u8; 32]))
            .expect("run");
        assert_eq!(out, vec!["verified"]);

        // A `Secret` parameter without a host-granted key is refused.
        let m2 = parser::parse_module("fn main(console: Console, s: Secret):\n    print(console, \"x\")\n").expect("parse");
        let l2 = crate::linker::link(vec![("main".into(), m2)], "main").expect("link");
        assert!(interpreter::run_module_signed(l2, ".", Vec::new(), Vec::new(), None).is_err());

        // The signing authority surfaces in the capability footprint.
        let fp = crate::capabilities::analyze(
            &parser::parse_module("fn main(console: Console, s: Secret):\n    print(console, \"x\")\n").expect("parse"),
        );
        assert!(fp.total.contains_key("Secret"), "Secret should appear in the footprint");
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

    /// `toml.table`/`keys`/`inline_get` enumerate a table whose keys aren't known
    /// ahead of time (`[dependencies]`, whose values are inline tables), and
    /// `array_tables` walks a `[[rune]]` array-of-tables (a `witchy.lock`) — the
    /// manifest+lock shapes a self-hosted package manager reads but `get` cannot.
    #[test]
    fn toml_module_enumerates_tables_and_arrays() {
        let src = r#"import toml
import string

fn main(console: Console):
    let m = "[rune]\nname = \"ledger\"\n\n[dependencies]\n\"money\" = { path = \"../money\" }\n\"acme/util\" = { path = \"../util\", version = \"1.2\" }\n"
    print(console, string.join(toml.keys(m, "dependencies"), "|"))
    print(console, opt(toml.inline_get("{ path = \"../money\" }", "path")))
    print(console, opt(toml.inline_get("{ path = \"../util\", version = \"1.2\" }", "version")))
    let lock = "[[rune]]\nname = \"money\"\nhash = \"sha256:aa\"\n\n[[rune]]\nname = \"util\"\nhash = \"sha256:bb\"\n"
    var names = []
    for block in toml.array_tables(lock, "rune"):
        names = list.push(names, opt(toml.get(block, "name")) <> "=" <> opt(toml.get(block, "hash")))
    print(console, string.join(names, "|"))

fn opt(o: Option(String)) -> String:
    match o:
        Some(s) -> s
        None -> "(none)"
"#;
        assert_eq!(
            link_run(src),
            vec![
                "money|acme/util",
                "../money",
                "1.2",
                "money=sha256:aa|util=sha256:bb"
            ]
        );
    }

    /// `std/semver` (pure witchy) parses `major.minor.patch`, matches the `^`/`~`/
    /// exact/`>=`/`*` constraints (rights matching the Rust resolver's semver),
    /// and picks the highest satisfying version — what dependency resolution needs.
    #[test]
    fn semver_module_parses_and_matches_constraints() {
        let src = r#"import semver

fn main(console: Console):
    print(console, yes(req_matches("^1.2.0", "1.9.9")))
    print(console, yes(req_matches("^1.2.0", "2.0.0")))
    print(console, yes(req_matches("^0.4.0", "0.5.0")))
    print(console, yes(req_matches("~1.2.0", "1.2.9")))
    print(console, yes(req_matches("~1.2.0", "1.3.0")))
    print(console, yes(req_matches(">=1.0.0", "3.0.0")))
    print(console, best_of("^1.2.0"))

fn req_matches(r: String, v: String) -> Bool:
    match semver.parse_req(r):
        Ok(req) -> match semver.parse(v):
            Ok(ver) -> semver.matches(req, ver)
            Err(e) -> false
        Err(e) -> false

fn best_of(r: String) -> String:
    let vs = [semver.version(1, 0, 0), semver.version(1, 2, 0), semver.version(1, 9, 9), semver.version(2, 0, 0)]
    match semver.parse_req(r):
        Ok(req) -> match semver.best(vs, req):
            Some(v) -> semver.format(v)
            None -> "(none)"
        Err(e) -> "err"

fn yes(b: Bool) -> String:
    if b: "y" else: "n"
"#;
        assert_eq!(
            link_run(src),
            vec!["y", "n", "n", "y", "n", "y", "1.9.9"]
        );
    }

    /// `std/path` does pure '/'-path surgery: base/dir/ext/stem, join (an absolute
    /// right-hand side replaces), and normalize (collapsing `.`/`..`, never
    /// escaping an absolute root, keeping leading `..` when relative).
    #[test]
    fn path_module_components_and_normalize() {
        let src = r#"import path

fn main(console: Console):
    print(console, path.base("a/b/c.txt") <> "|" <> path.dir("a/b/c.txt"))
    print(console, path.ext("a/b.tar.gz") <> "|" <> path.stem("a/b.tar.gz"))
    print(console, "[" <> path.ext(".bashrc") <> "]|" <> path.base("a/b/"))
    print(console, path.join("a/b", "c") <> "|" <> path.join("a", "/x"))
    print(console, path.normalize("a/./b/../c/") <> "|" <> path.normalize("/a/b/../../../x"))
    print(console, path.normalize("../a/../../b"))
"#;
        assert_eq!(
            link_run(src),
            vec![
                "c.txt|a/b",
                "gz|b.tar",
                "[]|b",
                "a/b/c|/x",
                "a/c|/x",
                "../../b",
            ]
        );
    }

    /// The committed `docs/stdlib.md` must match what `witchy doc` generates from
    /// the std sources — so a std module change that isn't re-documented fails
    /// loudly. Regenerate with: `witchy doc std/*.witchy > docs/stdlib.md`.
    #[test]
    fn stdlib_docs_are_current() {
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir("std")
            .expect("read std/")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("witchy"))
            .collect();
        files.sort();
        let mut generated = String::from("# API reference\n\n");
        for f in &files {
            let stem = f.file_stem().and_then(|s| s.to_str()).unwrap();
            let src = std::fs::read_to_string(f).unwrap();
            generated.push_str(&crate::doc::render(stem, &src).expect("render"));
        }
        let committed = std::fs::read_to_string("docs/stdlib.md").expect("read docs/stdlib.md");
        for (i, (g, c)) in generated.lines().zip(committed.lines()).enumerate() {
            assert_eq!(
                g,
                c,
                "docs/stdlib.md is stale at line {} — regenerate with `witchy doc std/*.witchy > docs/stdlib.md`",
                i + 1
            );
        }
        assert_eq!(
            generated.lines().count(),
            committed.lines().count(),
            "docs/stdlib.md length differs — regenerate with `witchy doc std/*.witchy > docs/stdlib.md`"
        );
    }

    /// `std/encoding` — hex + base64 over UTF-8 bytes (native, like crypto),
    /// matching the standard vectors incl. padding, and round-tripping multibyte
    /// UTF-8.
    #[test]
    fn encoding_module_hex_and_base64() {
        let src = r#"import encoding

fn main(console: Console):
    print(console, encoding.hex_encode("hello"))
    print(console, encoding.hex_decode("68656c6c6f"))
    print(console, encoding.base64_encode("Man"))
    print(console, encoding.base64_encode("Ma"))
    print(console, encoding.base64_decode("aGVsbG8="))
    print(console, yn(encoding.base64_decode(encoding.base64_encode("witchy! 🧙")) == "witchy! 🧙"))

fn yn(b: Bool) -> String:
    if b: "y" else: "n"
"#;
        assert_eq!(
            link_run(src),
            vec!["68656c6c6f", "hello", "TWFu", "TWE=", "hello", "y"]
        );
    }

    /// The `examples/time_and_encoding.witchy` showcase runs: a formatted civil
    /// date and base64/hex of a multibyte-UTF-8 payload, round-tripped — its
    /// footprint is just Console.
    #[test]
    fn time_and_encoding_example_runs() {
        assert_eq!(
            crate::execute_file("examples/time_and_encoding.witchy", Vec::new()).unwrap(),
            vec![
                "date:    2026-05-28T20:26:40Z (Thursday)",
                "layout:  Thursday, May 28 2026 at 20:26",
                "parsed:  2026-06-08T20:30:00Z",
                "checked: day 30 is out of range for 2026-2",
                "base64:  d2l0Y2h5IPCfp5k=",
                "hex:     77697463687920f09fa799",
                "decoded: witchy 🧙",
            ]
        );
        let src = std::fs::read_to_string("examples/time_and_encoding.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console");
    }

    /// `examples/calc.witchy` — a recursive-descent arithmetic evaluator — honors
    /// operator precedence and left-associativity, and reports division-by-zero
    /// and parse errors through `Result`. A pure (Console-only) tour of recursive
    /// enums + pattern matching.
    #[test]
    fn calc_example_evaluates_with_precedence_and_errors() {
        assert_eq!(
            crate::execute_file("examples/calc.witchy", Vec::new()).unwrap(),
            vec![
                "2 + 3 * 4       => 14",
                "(2 + 3) * 4     => 20",
                "100 - 2 - 3     => 95",
                "2 * (10 - 1)    => 18",
                "8 / (4 - 4)     => error: division by zero",
                "2 * (3 +        => error: unexpected end of input",
            ]
        );
        let src = std::fs::read_to_string("examples/calc.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console");
    }

    /// `examples/wrap.witchy` — greedy word wrapping — packs space-separated
    /// words onto lines within a column width, breaking before overflow, and
    /// frames each padded line. Pure string handling; agrees on both backends.
    #[test]
    fn wrap_example_greedily_wraps_to_width() {
        assert_eq!(
            crate::execute_file("examples/wrap.witchy", Vec::new()).unwrap(),
            vec![
                "wrapped to 20 columns:",
                "| The quick brown fox  |",
                "| jumps over the lazy  |",
                "| dog and then keeps   |",
                "| on running far away  |",
            ]
        );
    }

    /// `examples/dijkstra.witchy` — single-source shortest paths in a weighted
    /// directed graph — settles the nearest node, relaxes edges, then prints
    /// every distance and one reconstructed path. Returns a tuple of parallel
    /// arrays, so it also covers tuple-return + `let (a, b) =` on both backends.
    #[test]
    fn dijkstra_example_finds_shortest_paths() {
        assert_eq!(
            crate::execute_file("examples/dijkstra.witchy", Vec::new()).unwrap(),
            vec![
                "shortest distances from A:",
                "  A = 0",
                "  B = 3",
                "  C = 1",
                "  D = 4",
                "  E = 7",
                "path to E: A -> C -> B -> D -> E",
            ]
        );
    }

    /// `examples/queens.witchy` — N-queens by backtracking — counts all 92
    /// solutions for the 8x8 board and renders the first (column-order DFS). Deep
    /// recursion with an early-exit search; agrees on both backends.
    #[test]
    fn queens_example_counts_and_renders_first_board() {
        assert_eq!(
            crate::execute_file("examples/queens.witchy", Vec::new()).unwrap(),
            vec![
                "8-queens solutions: 92",
                "first solution:",
                "Q.......",
                "....Q...",
                ".......Q",
                ".....Q..",
                "..Q.....",
                "......Q.",
                ".Q......",
                "...Q....",
            ]
        );
    }

    /// `examples/anagram.witchy` — groups words that are letter-rearrangements
    /// of each other by a sorted-character signature, bucketing with a parallel
    /// signatures/groups list (no Dict). Exercises sorting characters (string
    /// `<`) and signature equality (string `==`) on both backends.
    #[test]
    fn anagram_example_groups_by_sorted_signature() {
        assert_eq!(
            crate::execute_file("examples/anagram.witchy", Vec::new()).unwrap(),
            vec!["listen, silent, enlist", "cat, act, tac", "dog, god"]
        );
    }

    /// `examples/stats.witchy` — summary statistics over a `List(Float)` —
    /// computes count/mean/variance/stddev/min/max, rendering with
    /// math.format_float. Floats live in the list and flow through arithmetic and
    /// sqrt; a guard that floats-in-collections + fixed-decimal formatting agree
    /// on both backends.
    #[test]
    fn stats_example_summarizes_a_float_list() {
        assert_eq!(
            crate::execute_file("examples/stats.witchy", Vec::new()).unwrap(),
            vec![
                "count    8",
                "mean     5.00",
                "variance 4.00",
                "stddev   2.00",
                "min      2.00",
                "max      9.00",
            ]
        );
    }

    /// `examples/regex.witchy` — a tiny K&P-style regex matcher (literals, `.`,
    /// `*`, `^`, `$`) — matches a battery of pattern/text pairs. Every step is a
    /// two-`list.at(..)` character comparison, so it stresses content comparison on
    /// both backends.
    #[test]
    fn regex_example_matches_literals_dot_star_anchors() {
        assert_eq!(
            crate::execute_file("examples/regex.witchy", Vec::new()).unwrap(),
            vec![
                "/abc/     \"abc\"           match",
                "/a.c/     \"axc\"           match",
                "/a.c/     \"ac\"            no match",
                "/a*b/     \"aaab\"          match",
                "/a*b/     \"b\"             match",
                "/^hello/  \"hello world\"   match",
                "/world$/  \"hello world\"   match",
                "/^a.*z$/  \"abcz\"          match",
                "/^a.*z$/  \"abc\"           no match",
            ]
        );
    }

    /// `region:` Phase 1 (docs/regions.md): the syntax parses (with optional
    /// `-> T` ascription), the block's value escapes, scalar outer
    /// assignments are allowed, and both backends agree — a region NEVER
    /// changes observable behavior, only when memory is reclaimed.
    #[test]
    fn region_blocks_value_escape_and_parity() {
        let src = "import string\n\nfn main(console: Console):\n    let summary = region:\n        var parts = []\n        for i in 0..50:\n            parts = list.push(parts, to_string(i))\n        string.join(parts, \",\")\n    print(console, to_string(string.length(summary)))\n    var n = 0\n    let direct = region -> Int:\n        n = n + 42\n        n\n    print(console, to_string(direct))\n";
        let want: Vec<String> = ["139", "42"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// `region:` Phase 2: the copy-out handles every shape — string, record
    /// with a nested list, recursive generic ADT, dict (whose hidden index is
    /// dropped on the way out), nested regions — and parent-side values pass
    /// through shared, all agreeing with the interpreter.
    #[test]
    fn region_copy_out_shapes_agree_on_both_backends() {
        let src = "type Stack:\n    Empty\n    Push(a, Stack(a))\n\ntype Reading:\n    sensor: String\n    values: List(Int)\n\nfn main(console: Console):\n    let st = region -> Stack(Int):\n        Push(1, Push(2, Empty))\n    print(console, to_string(st == Push(1, Push(2, Empty))))\n    let r = region -> Reading:\n        var vs = []\n        for i in 0..50:\n            vs = list.push(vs, i * i)\n        Reading(sensor: \"t\" <> \"0\", values: vs)\n    print(console, r.sensor)\n    print(console, to_string(list.at(r.values, 49)))\n    let d = region -> Dict(String, Int):\n        var m = dict.new()\n        for i in 0..100:\n            m = dict.insert(m, \"k\" <> to_string(i), i)\n        m\n    print(console, to_string(dict.get_or(d, \"k42\", 0 - 1)))\n    let shared = \"parent-side\"\n    let s = region -> String:\n        shared\n    print(console, s)\n    let nested = region -> Int:\n        let inner = region -> String:\n            \"abc\" <> \"def\"\n        string.length(inner)\n    print(console, to_string(nested))\n";
        let want: Vec<String> = ["true", "t0", "2401", "42", "parent-side", "6"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// `region:` reclamation: 100k regions each churning a 1000-element
    /// throwaway list (~1.6 GB cumulative, past the 1 GB cap) run in constant
    /// memory — inside a loop the automatic reset cannot help (an outer list
    /// grows), so only the region machinery reclaims. WASM-only for speed.
    #[test]
    fn region_reclaims_inside_nonresettable_loops() {
        let src = "fn main(console: Console):\n    var total = 0\n    var keep = []\n    for i in 0..100000:\n        let last = region -> Int:\n            var row = []\n            var j = 0\n            for j in 0..1000:\n                row = list.push(row, j)\n            list.at(row, 999)\n        total = total + last\n        keep = list.push(keep, i)\n    print(console, to_string(total))\n    print(console, to_string(list.length(keep)))\n";
        assert_eq!(wasm_run(src), vec!["99900000", "100000"]);
    }

    /// `region:` Phase 3: the `__region_copy_bytes` counter proves the
    /// watermark short-circuit — a parent-side passthrough copies ZERO bytes,
    /// and a region-born string copies exactly its own block.
    #[test]
    fn region_copy_counter_proves_passthrough_is_free() {
        use crate::runtime::{Capabilities, Runtime};
        let run_and_count = |src: &str| -> (Vec<String>, i64) {
            let module = parser::parse_module(src).expect("parse");
            let wat = codegen::compile_module(&module).expect("compile");
            let mut rt = Runtime::batch().expect("rt");
            let mut actor = rt
                .spawn(
                    wat.as_bytes(),
                    Capabilities { print: true, quiet: true, ..Default::default() },
                    64,
                )
                .expect("spawn");
            actor.run().expect("run");
            (actor.output(), actor.region_copy_bytes().expect("counter"))
        };
        // Parent-side value: shared, not copied.
        let (out, copied) = run_and_count(
            "fn main(console: Console):\n    let shared = \"twelve chars\"\n    let s = region -> String:\n        shared\n    print(console, s)\n",
        );
        assert_eq!(out, vec!["twelve chars"]);
        assert_eq!(copied, 0, "parent passthrough must copy nothing");
        // Region-born value: exactly its own block (4-byte header + 6 bytes).
        let (out, copied) = run_and_count(
            "fn main(console: Console):\n    let s = region -> String:\n        \"abc\" <> \"def\"\n    print(console, s)\n",
        );
        assert_eq!(out, vec!["abcdef"]);
        assert_eq!(copied, 10, "a region-born string copies header + bytes");
    }

    /// `region:` rejections: an outer pointer-typed assignment and a `yield`
    /// are type errors — the region's only pointer escape is its value.
    #[test]
    fn region_rejects_outer_pointer_assign_and_yield() {
        let leak = "fn main(console: Console):\n    var leak = [1]\n    let x = region:\n        leak = list.push(leak, 2)\n        7\n    print(console, to_string(x))\n";
        let module = parser::parse_module(leak).expect("parse");
        let linked = crate::linker::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked).expect_err("outer pointer assign must be rejected");
        assert!(err.to_string().contains("inside `region:`"), "{err}");
    }
    /// open-addressing table over the (insertion-ordered) entry array, so
    /// get_or/has/insert lookups probe instead of scanning. String and Int
    /// keys, growth rebuilds, removal (index dropped, rebuilt on next
    /// growth), and a missing-key probe all agree with the interpreter.
    #[test]
    fn dict_hash_index_agrees_on_both_backends() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    for i in 0..3000:\n        d = dict.insert(d, \"k\" <> to_string(i), i * 2)\n    print(console, to_string(dict.size(d)))\n    print(console, to_string(dict.get_or(d, \"k2999\", 0 - 1)))\n    print(console, to_string(dict.get_or(d, \"absent\", 0 - 1)))\n    print(console, to_string(dict.has(d, \"k1500\")))\n    d = dict.remove(d, \"k0\")\n    print(console, to_string(dict.size(d)))\n    d = dict.insert(d, \"again\", 7)\n    print(console, to_string(dict.get_or(d, \"again\", 0 - 1)))\n";
        let want: Vec<String> = ["3000", "5998", "-1", "true", "2999", "7"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// IN-PLACE DICT INSERT: `d = dict.insert(d, k, v)` updates/appends into owned
    /// entry slack (no per-insert table copy); an aliased dict keeps the
    /// copying insert, so the alias still sees the original.
    #[test]
    fn inplace_dict_insert_is_fast_and_alias_safe() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    for i in 0..2000:\n        d = dict.insert(d, i, i * 2)\n    print(console, to_string(dict.size(d)))\n    print(console, to_string(dict.get_or(d, 1999, 0 - 1)))\n    var e = dict.new()\n    let alias = e\n    e = dict.insert(e, 1, 10)\n    print(console, to_string(dict.size(alias)))\n    print(console, to_string(dict.size(e)))\n";
        let want: Vec<String> =
            ["2000", "3998", "0", "1"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// ARENA WATERMARK RESETS: a loop whose body lets nothing escape an
    /// iteration (only scalar outer assignments) reclaims each iteration's
    /// allocations — 200k iterations that would otherwise demand ~6 GB run in
    /// constant memory. WASM-only: the interpreter's clone-per-push is
    /// quadratic and would take far too long at this scale.
    #[test]
    fn arena_resets_keep_escape_free_loops_constant_memory() {
        let src = "fn main(console: Console):\n    var total = 0\n    for i in 0..200000:\n        var row = []\n        var j = 0\n        for j in 0..1000:\n            row = list.push(row, j)\n        total = total + list.at(row, 999)\n    print(console, to_string(total))\n";
        assert_eq!(wasm_run(src), vec!["199800000"]);
    }

    /// IN-PLACE STRING APPEND: the builder pattern `s = s <> piece` appends
    /// into owned byte slack (amortized O(1)); a literal-seeded alias keeps
    /// the copying path, so the interned literal is never mutated.
    #[test]
    fn inplace_string_append_is_fast_and_alias_safe() {
        let src = "fn main(console: Console):\n    var s = \"\"\n    for i in 0..20000:\n        s = s <> \"ab\"\n    print(console, to_string(string.length(s)))\n    var t = \"seed\"\n    let alias = t\n    t = t <> \"!\"\n    print(console, alias)\n    print(console, t)\n";
        let want: Vec<String> =
            ["40000", "seed", "seed!"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// IN-PLACE PUSH (the linear-update optimization): an unaliased
    /// accumulate-in-loop appends into owned slack — 50k pushes complete
    /// instantly instead of O(n²) copying — while an ALIASED list keeps the
    /// copying push, so value semantics hold: `ys` still sees the original.
    #[test]
    fn inplace_push_is_fast_and_alias_safe() {
        // 50k would take minutes under clone-per-push on either backend; both
        // have an in-place fast path for the unaliased self-assign shape.
        let src = "fn main(console: Console):\n    var xs = []\n    for i in 0..50000:\n        xs = list.push(xs, i)\n    print(console, to_string(list.length(xs)))\n    print(console, to_string(list.at(xs, 49999)))\n    var small = [1]\n    let alias = small\n    small = list.push(small, 2)\n    print(console, to_string(alias))\n    print(console, to_string(small))\n";
        let want: Vec<String> = ["50000", "49999", "[1]", "[1, 2]"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// IN-PLACE DICT ACCUMULATION: the `d = dict.insert(d, k, v)` and
    /// `d = dict.update(d, k, dflt, f)` self-assign shapes mutate the slot in place
    /// on both backends; an aliased dict keeps the copying path so value
    /// semantics hold.
    #[test]
    fn inplace_dict_upsert_is_fast_and_alias_safe() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    for i in 0..10000:\n        d = dict.insert(d, i, i)\n    print(console, to_string(dict.size(d)))\n    var counts = dict.new()\n    for i in 0..30000:\n        counts = dict.update(counts, i % 3, 0, fn(n: Int): n + 1)\n    print(console, to_string(dict.get_or(counts, 0, 0)))\n    var small = dict.new()\n    small = dict.insert(small, 1, 10)\n    let alias = small\n    small = dict.insert(small, 2, 20)\n    print(console, to_string(dict.size(alias)))\n    print(console, to_string(dict.size(small)))\n";
        let want: Vec<String> = ["10000", "10000", "1", "2"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// Closure capture is pruned to the names the body mentions (the
    /// interpreter used to clone the entire environment per closure — itself a
    /// quadratic cost in accumulation loops). Calling through a captured
    /// closure variable still works, and capture remains a snapshot: a later
    /// reassignment of the source variable is invisible to the closure.
    #[test]
    fn closure_capture_pruned_and_snapshot() {
        let src = "fn main(console: Console):\n    let add = fn(x: Int): x + 1\n    let twice = fn(y: Int): add(add(y))\n    print(console, to_string(twice(3)))\n    var n = 10\n    let snap = fn(): n\n    n = 99\n    print(console, to_string(snap()))\n";
        let want: Vec<String> = ["5", "10"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// The `std/regex` toolkit — greedy quantifiers, escapes (`\d`/`\w`/`\s` and
    /// literal metacharacters), character classes with ranges and negation, and
    /// the span-based API (`find`/`find_all`/`extract`/`replace_all`/`split`) —
    /// agrees on both backends, including the `Option((Int, Int))` span payload.
    #[test]
    fn regex_module_toolkit_agrees_on_both_backends() {
        let src = "import regex\nimport string\n\nfn main(console: Console):\n    print(console, to_string(regex.matches(\"h.llo\", \"say hello\")))\n    print(console, to_string(regex.matches(\"^\\\\d+$\", \"12345\")))\n    print(console, to_string(regex.matches(\"^\\\\d+$\", \"12a45\")))\n    print(console, to_string(regex.extract(\"\\\\d+\", \"a1b22c333\")))\n    print(console, regex.replace_all(\"\\\\s+\", \"too   many    spaces\", \" \"))\n    print(console, to_string(regex.split(\",\\\\s*\", \"a, b,c\")))\n    print(console, to_string(regex.matches(\"[a-f]+\", \"deadbeef\")))\n    print(console, to_string(regex.matches(\"^[^0-9]+$\", \"abc\")))\n    print(console, to_string(regex.find(\"a+\", \"caat\")))\n    print(console, to_string(regex.matches(\"\\\\w+@\\\\w+\\\\.\\\\w+\", \"mail me: a_b@example.com\")))\n    print(console, regex.replace_all(\"[0-9]+\", \"r2d2\", \"#\"))\n";
        let want: Vec<String> = [
            "true",
            "true",
            "false",
            "[1, 22, 333]",
            "too many spaces",
            "[a, b, c]",
            "true",
            "true",
            "Some((1, 3))",
            "true",
            "r#d#",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// `examples/matrix.witchy` — integer matrices — multiplies a 2x3 by a 3x2,
    /// transposes, and prints an identity, all with right-aligned columns. A
    /// `List(List(Int))` workout (nested `at`) that agrees on both backends.
    #[test]
    fn matrix_example_multiplies_and_transposes() {
        assert_eq!(
            crate::execute_file("examples/matrix.witchy", Vec::new()).unwrap(),
            vec![
                "A x B =",
                "[  58  64 ]",
                "[ 139 154 ]",
                "transpose(A) =",
                "[ 1 4 ]",
                "[ 2 5 ]",
                "[ 3 6 ]",
                "identity(3) =",
                "[ 1 0 0 ]",
                "[ 0 1 0 ]",
                "[ 0 0 1 ]",
            ]
        );
    }

    /// `examples/brainfuck.witchy` — a full brainfuck interpreter — runs the
    /// canonical "Hello World!" program and a second that prints 'A', building
    /// output by indexing a printable-ASCII literal (no chr/ord builtin). The
    /// instruction dispatch compares `list.at(code, pc)` against operator literals,
    /// so it's another both-backends guard for content comparison.
    #[test]
    fn brainfuck_example_runs_hello_world() {
        assert_eq!(
            crate::execute_file("examples/brainfuck.witchy", Vec::new()).unwrap(),
            vec!["Hello World!", "A"]
        );
    }

    /// `examples/diff.witchy` — an LCS line diff — fills the longest-common-
    /// subsequence table and backtracks into unchanged/removed/added lines. The
    /// backtrack compares `list.at(old, i) == list.at(new, j)` (two `List(String)` element
    /// reads), so it also guards content comparison on both backends.
    #[test]
    fn diff_example_emits_lcs_line_diff() {
        assert_eq!(
            crate::execute_file("examples/diff.witchy", Vec::new()).unwrap(),
            vec![
                "  apple",
                "- banana",
                "  cherry",
                "  date",
                "+ elderberry",
                "  fig",
            ]
        );
    }

    /// `examples/toposort.witchy` — Kahn's topological sort over a dependency
    /// graph — produces a valid build order and reports a cycle. Pure (Console),
    /// list-based (no Dict), both backends.
    #[test]
    fn toposort_example_orders_and_detects_cycles() {
        assert_eq!(
            crate::execute_file("examples/toposort.witchy", Vec::new()).unwrap(),
            vec![
                "build order: boot -> config -> db -> cache -> api -> web",
                "cyclic:      error: cycle among egg, chicken",
            ]
        );
    }

    /// `examples/jq.witchy` — a JSON query tool — walks a dotted path (object keys
    /// and numeric array indices) into a decoded document and renders the value.
    /// Pure (Console), both backends.
    #[test]
    fn jq_example_queries_json_by_path() {
        assert_eq!(
            crate::execute_file("examples/jq.witchy", Vec::new()).unwrap(),
            vec![
                "user.name       => \"Ada\"",
                "user.roles      => [\"admin\",\"dev\"]",
                "user.roles.0    => \"admin\"",
                "user.roles.1    => \"dev\"",
                "count           => 42",
                "active          => true",
                "user.missing    => (no such path)",
            ]
        );
    }

    /// `examples/rpn.witchy` — a stack-machine reverse-Polish calculator — folds
    /// tokens through an operand stack and reports underflow / division-by-zero
    /// through `Result`. Pure (Console), both backends.
    #[test]
    fn rpn_example_evaluates_postfix_with_a_stack() {
        assert_eq!(
            crate::execute_file("examples/rpn.witchy", Vec::new()).unwrap(),
            vec![
                "3 4 +               => 7",
                "5 1 2 + 4 * + 3 -   => 14",
                "10 2 /              => 5",
                "1 0 /               => error: division by zero",
                "1 +                 => error: stack underflow at `+`",
            ]
        );
    }

    /// `examples/maze.witchy` — BFS shortest path through a grid maze, with a
    /// `prev` Dict for path reconstruction. Pure (Console); interpreter-hosted.
    #[test]
    fn maze_example_finds_shortest_path_by_bfs() {
        let out = crate::execute_file("examples/maze.witchy", Vec::new())
            .unwrap()
            .join("\n");
        assert!(out.contains("shortest path: 14 steps"), "distance: {out}");
        assert!(
            out.contains("#S#***# #") && out.contains("### ###*#"),
            "route marked: {out}"
        );
    }

    /// `examples/traits.witchy` — defines a custom `Shape` trait, implements it for
    /// three types, and dispatches generically (`where s: Shape`). Monomorphized,
    /// so it runs identically on both backends.
    #[test]
    fn traits_example_dispatches_a_custom_trait() {
        assert_eq!(
            crate::execute_file("examples/traits.witchy", Vec::new()).unwrap(),
            vec![
                "square with area 25",
                "rectangle with area 12",
                "right triangle with area 12",
                "total of three squares: 29",
            ]
        );
    }

    /// `examples/sudoku.witchy` — a backtracking solver over immutable boards —
    /// solves the canonical puzzle to its unique solution. Pure (Console),
    /// recursion + Option-backtracking heavy.
    #[test]
    fn sudoku_example_solves_by_backtracking() {
        let out = crate::execute_file("examples/sudoku.witchy", Vec::new())
            .unwrap()
            .join("\n");
        assert!(
            out.contains("solved:\n534678912\n672195348\n198342567\n859761423"),
            "unique solution: {out}"
        );
        let src = std::fs::read_to_string("examples/sudoku.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console");
    }

    /// `examples/life.witchy` — Conway's Game of Life over a `List(List(Bool))` —
    /// evolves a glider through its phases by the B3/S23 rule. Pure (Console),
    /// nested-list heavy, and identical on both backends.
    #[test]
    fn life_example_evolves_a_glider() {
        let out = crate::execute_file("examples/life.witchy", Vec::new())
            .unwrap()
            .join("\n");
        // Generation 0 is the seeded glider.
        assert!(
            out.contains("generation 0:\n.#......\n..#.....\n###....."),
            "seed glider: {out}"
        );
        // After 3 steps it has drifted down-and-right into its next phase.
        assert!(
            out.contains("generation 3:\n........\n.#......\n..##....\n.##....."),
            "evolved glider: {out}"
        );
        let src = std::fs::read_to_string("examples/life.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console");
    }

    /// Regression (found by `examples/calc.witchy` via the both-backends invariant):
    /// comparing a String whose type isn't locally tracked — a List(String)
    /// element via `at` — to a literal must be a *structural* `$str_eq` on the
    /// WASM backend, not a pointer compare, with the literal on either side.
    #[test]
    fn wasm_string_eq_uses_str_eq_when_literal_on_either_side() {
        let src = "fn main(console: Console):\n    let cs = [\"a\", \" \", \"z\"]\n    print(console, if list.at(cs, 1) == \" \": \"eq\" else: \"ne\")\n    print(console, if \"a\" == list.at(cs, 0): \"eq\" else: \"ne\")\n    print(console, if list.at(cs, 0) == \"z\": \"eq\" else: \"ne\")\n";
        let want = vec!["eq".to_string(), "eq".to_string(), "ne".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// Comparing two `list.at(list, i)` results — where neither operand is a literal —
    /// must compare String *content* on WASM, not pointers. The list holds two
    /// runtime-built (concatenated) strings with equal content but distinct heap
    /// addresses, so a pointer comparison would wrongly report "ne". Codegen now
    /// carries a `List(String)`'s element value type to `list.at(...)`, so `==` lowers
    /// to `$str_eq`. (Regression for the run-length-encoding parity divergence.)
    #[test]
    fn wasm_string_eq_on_two_at_results_compares_content() {
        let src = "fn main(console: Console):\n    let a = \"x\" <> \"y\"\n    let b = \"x\" <> \"y\"\n    let xs = [a, b, \"zz\"]\n    print(console, if list.at(xs, 0) == list.at(xs, 1): \"eq\" else: \"ne\")\n    print(console, if list.at(xs, 0) == list.at(xs, 2): \"eq\" else: \"ne\")\n";
        let want = vec!["eq".to_string(), "ne".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// A negative `Int` that enters a list through a *generic* function (the
    /// element type is a type variable, so it crosses the i32 generic ABI) and is
    /// then read back through *concrete* `List(Int)` code must keep its sign on
    /// WASM. `to_slot` used to zero-extend, turning -1 into 4294967295 when a
    /// concrete reader loaded the i64 slot; it now sign-extends (pointers/Bools
    /// are < 2^31, so they're unaffected). Regression for the generic-list bug
    /// found via `list.repeat(-1, n)`.
    #[test]
    fn wasm_negative_int_survives_the_generic_list_abi() {
        let src = "fn fill(x: a, n: Int) -> List(a):\n    var out = []\n    var i = 0\n    while i < n:\n        out = list.push(out, x)\n        i = i + 1\n    out\n\nfn show(xs: List(Int)) -> String:\n    var out = \"\"\n    for v in xs:\n        out = out <> to_string(v) <> \" \"\n    out\n\nfn main(console: Console):\n    print(console, show(fill(-1, 3)))\n";
        let want = vec!["-1 -1 -1 ".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// An *unbounded* generic function that compares its type-variable values
    /// (`x == target`) must compare String CONTENT on WASM, not pointers. The
    /// WASM backend monomorphizes the call on the concrete element type
    /// (`count_eq__String`), so `==` lowers to `$str_eq`. The strings are built at
    /// runtime (distinct pointers, equal content) so a pointer compare would give
    /// the wrong count. (Regression for the generic-`==`-on-non-primitives gap.)
    #[test]
    fn wasm_monomorphizes_generic_equality_on_strings() {
        let src = "fn count_eq(xs: List(a), target: a) -> Int:\n    var n = 0\n    for x in xs:\n        if x == target:\n            n = n + 1\n    n\n\nfn b(s: String) -> String:\n    s <> \"\"\n\nfn main(console: Console):\n    print(console, to_string(count_eq([b(\"aa\"), b(\"bb\"), b(\"aa\")], b(\"aa\"))))\n";
        let want = vec!["2".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// A large `Int` carried through an *unbounded* generic function must keep its
    /// 64 bits on WASM. The generic i32 ABI truncated it; the WASM backend now
    /// monomorphizes the call on `Int` (`fill__Int`), so the i64 survives.
    /// (Regression for the big-int-through-generic gap.)
    #[test]
    fn wasm_monomorphizes_big_int_through_generic() {
        let src = "fn fill(x: a, n: Int) -> List(a):\n    var out = []\n    var i = 0\n    while i < n:\n        out = list.push(out, x)\n        i = i + 1\n    out\n\nfn main(console: Console):\n    let xs = fill(5000000000, 2)\n    print(console, to_string(list.at(xs, 0)))\n";
        let want = vec!["5000000000".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// A large `Int` RETURNED from a closure must keep its 64 bits on WASM.
    /// Closures use the i64 universal-slot result ABI, and a higher-order call
    /// recovers the result at the closure's return kind (here `fn(Int) -> Int`).
    /// (Regression for the big-Int-through-closure-return gap.)
    #[test]
    fn wasm_big_int_returned_from_closure() {
        let src = "fn apply(f: fn(Int) -> Int, x: Int) -> Int:\n    f(x)\n\nfn main(console: Console):\n    print(console, to_string(apply(fn(k: Int): k * 5000000000, 2)))\n";
        let want = vec!["10000000000".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// A large `Int` passed AS a closure argument, and one CAPTURED by a closure,
    /// must keep their 64 bits on WASM. Closure params and captures use the i64
    /// universal slot (recovered at their kind in the lambda prologue), matching
    /// the result ABI. (Regression for big-Int-through-closure arg/capture.)
    #[test]
    fn wasm_big_int_closure_arg_and_capture() {
        // Argument: 5000000000 passed to the closure, + 1.
        let arg = "fn apply(f: fn(Int) -> Int, x: Int) -> Int:\n    f(x)\n\nfn main(console: Console):\n    print(console, to_string(apply(fn(k: Int): k + 1, 5000000000)))\n";
        assert_eq!(interp(arg), vec!["5000000001"], "interpreter (arg)");
        assert_eq!(run_on_wasm(arg), vec!["5000000001"], "WASM (arg)");
        // Capture: a big Int captured by the closure, recovered from the env.
        let cap = "fn apply(f: fn(Int) -> Int, x: Int) -> Int:\n    f(x)\n\nfn main(console: Console):\n    let big = 5000000000\n    print(console, to_string(apply(fn(x: Int): x + big, 1)))\n";
        assert_eq!(interp(cap), vec!["5000000001"], "interpreter (capture)");
        assert_eq!(run_on_wasm(cap), vec!["5000000001"], "WASM (capture)");
    }

    /// A Dict keyed by `Float` must look up the same on both backends. Float keys
    /// go into the universal i64 slot as their bit pattern; `$key_eq` mode 2
    /// reinterprets and compares with `f64.eq`, matching the interpreter's `==`
    /// (insertion-order, value equality). (Regression for the interpreter-only
    /// Float-key gap.)
    #[test]
    fn dict_float_keys_agree_on_both_backends() {
        let src = "fn main(console: Console):\n    let d = dict.insert(dict.insert(dict.insert(dict.new(), 1.5, \"a\"), 2.5, \"b\"), 1.5, \"c\")\n    print(console, dict.get_or(d, 1.5, \"?\"))\n    print(console, dict.get_or(d, 2.5, \"?\"))\n    print(console, dict.get_or(d, 9.9, \"?\"))\n    print(console, to_string(dict.size(d)))\n    let e = dict.remove(d, 1.5)\n    print(console, dict.get_or(e, 1.5, \"gone\"))\n    print(console, to_string(dict.size(e)))\n";
        let want = vec![
            "c".to_string(),
            "b".to_string(),
            "?".to_string(),
            "2".to_string(),
            "gone".to_string(),
            "1".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// The Secret capability is enforced in the WASM sandbox: with the same
    /// seed granted, sign/public_key/verify produce byte-identical results on
    /// both backends (Ed25519 is deterministic), and a module importing the
    /// signing ops cannot instantiate without the grant — the seed never enters
    /// guest memory.
    #[test]
    fn signing_key_compiles_to_wasm_and_is_gated() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "import crypto\nfn main(console: Console, signer: Secret):\n    let msg = \"sign me\"\n    let sig = crypto.sign(signer, msg)\n    print(console, crypto.public_key(signer))\n    print(console, sig)\n    print(console, if crypto.ed25519_verify(crypto.public_key(signer), msg, sig): \"verified\" else: \"FAILED\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::linker::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let seed = [7u8; 32];
        let interp_out =
            interpreter::run_module_signed(linked.clone(), ".", Vec::new(), Vec::new(), Some(seed))
                .expect("interp");
        assert_eq!(interp_out[2], "verified");
        let wat = codegen::compile_module(&linked).expect("compile");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                wat.as_bytes(),
                Capabilities {
                    print: true,
                    quiet: true,
                    signing_key: Some(seed),
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), interp_out, "signature + pubkey must be byte-identical");

        // Ungranted: the imports are absent, so instantiation fails.
        let mut rt = Runtime::batch().expect("runtime");
        let denied = rt.spawn(
            wat.as_bytes(),
            Capabilities { print: true, quiet: true, ..Default::default() },
            64,
        );
        assert!(denied.is_err(), "signing imports must not instantiate without the grant");
    }

    /// Every ```witchy code block in the documentation must be a real program:
    /// it parses, links, and type-checks; and when it defines a `main` whose
    /// footprint needs nothing beyond Console, it RUNS on both backends and the
    /// outputs must agree. Docs that drift from the language break the build.
    #[test]
    fn documentation_examples_are_valid() {
        let mut files: Vec<std::path::PathBuf> = vec![
            "README.md".into(),
            "CONTRIBUTING.md".into(),
            "examples/README.md".into(),
        ];
        for dir in ["docs", "book/src"] {
            let Ok(entries) = std::fs::read_dir(dir) else { continue };
            let mut md: Vec<_> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("md"))
                .collect();
            md.sort();
            files.extend(md);
        }

        let mut checked = 0usize;
        let mut ran = 0usize;
        for file in &files {
            let Ok(text) = std::fs::read_to_string(file) else { continue };
            for (idx, snippet) in extract_witchy_blocks(&text).into_iter().enumerate() {
                let context = format!("{}: ```witchy block #{}", file.display(), idx + 1);
                let module = parser::parse_module(&snippet)
                    .unwrap_or_else(|e| panic!("{context} fails to parse: {e:?}\n---\n{snippet}"));
                let linked = crate::linker::link(vec![("main".into(), module)], "main")
                    .unwrap_or_else(|e| panic!("{context} fails to link: {e}\n---\n{snippet}"));
                typeck::check(&linked)
                    .unwrap_or_else(|e| panic!("{context} fails to type-check: {e}\n---\n{snippet}"));
                checked += 1;

                let has_main = linked
                    .items
                    .iter()
                    .any(|it| matches!(it, ast::Item::Function(f) if f.name == "main"));
                // Actors compile through a separate module path (and run on the
                // demo scheduler), so the single-module run below doesn't apply —
                // such examples are still fully parse + type-checked above.
                let has_actor = linked.items.iter().any(|it| matches!(it, ast::Item::Actor(_)));
                // A `main` that declares an argv parameter (`args: List(String)`)
                // is type-checked but not run here: argv isn't a capability (so the
                // footprint still looks "Console-only"), yet the interpreter and
                // WASM run paths don't share an argv source, so comparing their
                // output is meaningless. Same rationale as the actor skip above.
                let reads_argv = linked.items.iter().any(|it| {
                    matches!(it, ast::Item::Function(f) if f.name == "main"
                        && f.params.iter().any(|p| matches!(&p.ty,
                            Some(ast::Type::Named(n, args)) if n == "List"
                                && matches!(args.first(),
                                    Some(ast::Type::Named(s, _)) if s == "String"))))
                });
                let footprint = crate::capabilities::analyze(&linked);
                let console_only = footprint.total.keys().all(|k| *k == "Console");
                if has_main && console_only && !has_actor && !reads_argv {
                    let wat = codegen::compile_module(&linked)
                        .unwrap_or_else(|e| panic!("{context} fails to compile to WASM: {e}"));
                    let interp =
                        interpreter::run_module(linked, std::path::Path::new("."), Vec::new())
                            .unwrap_or_else(|e| panic!("{context} fails on the interpreter: {e}"));
                    let compiled = crate::run_wat_capture(&wat)
                        .unwrap_or_else(|e| panic!("{context} fails on WASM: {e}"));
                    assert_eq!(interp, compiled, "{context}: the backends DIVERGE");
                    ran += 1;
                }
            }
        }
        assert!(checked >= 20, "expected the docs to carry many checked examples, found {checked}");
        assert!(ran >= 5, "expected several runnable doc examples, found {ran}");
    }

    /// Pull the contents of every fenced block tagged exactly `witchy`.
    fn extract_witchy_blocks(markdown: &str) -> Vec<String> {
        let mut blocks = Vec::new();
        let mut current: Option<String> = None;
        for line in markdown.lines() {
            match &mut current {
                None if line.trim_end() == "```witchy" => current = Some(String::new()),
                Some(body) => {
                    if line.trim_end() == "```" {
                        blocks.push(current.take().unwrap());
                    } else {
                        body.push_str(line);
                        body.push('\n');
                    }
                }
                None => {}
            }
        }
        blocks
    }

    /// The in-language test framework: `witchy test` discovers zero-parameter
    /// `test_*` functions, a test passes by returning and fails by aborting
    /// (std/testing's assertions report a message). Capability-free by
    /// construction.
    #[test]
    fn in_language_test_framework_runs_and_reports() {
        let path = std::env::temp_dir().join(format!("witchy_testfw_{}.witchy", std::process::id()));
        std::fs::write(
            &path,
            "import testing\n\nfn double(n: Int) -> Int:\n    n * 2\n\nfn test_double():\n    testing.assert_int_eq(double(21), 42)\n\nfn test_strings():\n    testing.assert_eq(\"a\" <> \"b\", \"ab\")\n    testing.assert_ne(\"a\", \"b\")\n\nfn test_broken():\n    testing.assert(1 > 2, \"deliberately wrong\")\n",
        )
        .unwrap();
        let (passed, failed) = crate::run_tests_in_file(path.to_str().unwrap()).expect("run");
        assert_eq!(passed.len(), 2, "two passing tests: {passed:?}");
        assert_eq!(failed.len(), 1, "one failing test: {failed:?}");
        assert!(failed[0].0.ends_with("test_broken"));
        assert!(
            failed[0].1.contains("deliberately wrong"),
            "failure carries the assertion message: {}",
            failed[0].1
        );
        let _ = std::fs::remove_file(&path);
    }

    /// `fail` is the loud abort on BOTH backends: a runtime error in the
    /// interpreter, a trap in compiled code.
    #[test]
    fn fail_aborts_on_both_backends() {
        let src = "fn main(console: Console):\n    print(console, \"before\")\n    fail(\"boom\")\n    print(console, \"after\")\n";
        let err = interpreter::run(src).expect_err("interpreter must abort");
        assert!(err.message.contains("boom"));
        let module = parser::parse_module(src).expect("parse");
        let wat = codegen::compile_module(&module).expect("compile");
        assert!(crate::run_wat_capture(&wat).is_err(), "WASM must trap on fail()");
    }

    /// `now` (Clock) and `get_env` (Env) compile to capability-gated host
    /// imports. `get_env` is deterministic given the process env, so both
    /// backends must agree exactly; `now` is wall-clock, so each backend is
    /// checked for plausibility instead. Also exercises a multi-capability
    /// `main` (Console + Env / Console + Clock), which codegen now accepts.
    #[test]
    fn clock_and_env_compile_to_wasm_and_agree() {
        // SAFETY-free env set: std::env::set_var is fine in a single-threaded
        // test context; the var is namespaced to this test.
        unsafe { std::env::set_var("WITCHY_E2E_ENV_VAR", "from the host") };
        let env_src = "import option\n\nfn main(console: Console, env: Env):\n    match get_env(env, \"WITCHY_E2E_ENV_VAR\"):\n        Some(v) -> print(console, \"got: \" <> v)\n        None -> print(console, \"unset\")\n    match get_env(env, \"WITCHY_E2E_DEFINITELY_UNSET\"):\n        Some(v) -> print(console, \"got: \" <> v)\n        None -> print(console, \"unset\")\n";
        let want = vec!["got: from the host".to_string(), "unset".to_string()];
        let module = parser::parse_module(env_src).expect("parse");
        let linked = crate::linker::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let wat = codegen::compile_module(&linked).expect("compile");
        assert_eq!(link_run(env_src), want.clone(), "interpreter");
        assert_eq!(crate::run_wat_capture(&wat).expect("wasm"), want, "compiled WASM must agree");

        // The clock: both backends must yield a plausible epoch-milliseconds.
        let clock_src = "fn main(console: Console, clock: Clock):\n    print(console, if now(clock) > 1500000000000: \"plausible\" else: \"implausible\")\n";
        assert_eq!(interp(clock_src), vec!["plausible"], "interpreter");
        assert_eq!(run_on_wasm(clock_src), vec!["plausible"], "compiled WASM");
    }

    /// The full Dir family compiles to capability-gated host imports and agrees
    /// with the interpreter: read/exists/is_dir/subdir/write/make_dir/list all
    /// round-trip in a confined temp directory, and escape attempts (`..`,
    /// absolute paths) FAIL on both backends.
    #[test]
    fn dir_capability_compiles_to_wasm_and_confines() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_wasm_dir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).expect("mkdir");
        std::fs::write(root.join("a.txt"), "alpha").expect("seed a");
        std::fs::write(root.join("sub/b.txt"), "beta").expect("seed b");

        let src = "fn main(console: Console, dir: Dir):\n    print(console, read(dir, \"a.txt\"))\n    print(console, to_string(exists(dir, \"a.txt\")))\n    print(console, to_string(exists(dir, \"missing.txt\")))\n    let sub = subdir(dir, \"sub\")\n    print(console, read(sub, \"b.txt\"))\n    write(dir, \"out.txt\", \"written\")\n    print(console, read(dir, \"out.txt\"))\n    make_dir(dir, \"made\")\n    print(console, to_string(is_dir(dir, \"made\")))\n    for name in list(dir):\n        print(console, \"entry: \" <> name)\n";
        let want = vec![
            "alpha".to_string(),
            "true".to_string(),
            "false".to_string(),
            "beta".to_string(),
            "written".to_string(),
            "true".to_string(),
            "entry: a.txt".to_string(),
            "entry: made".to_string(),
            "entry: out.txt".to_string(),
            "entry: sub".to_string(),
        ];
        let interp_out = interpreter::run_in(src, &root).expect("interp");
        assert_eq!(interp_out, want, "interpreter");
        let module = parser::parse_module(src).expect("parse");
        let wat = codegen::compile_module(&module).expect("compile");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                wat.as_bytes(),
                Capabilities {
                    print: true,
                    quiet: true,
                    dir_root: Some(root.clone()),
                    dir_read: true,
                    dir_write: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");

        for bad in ["../outside.txt", "/etc/hosts"] {
            let esc = format!(
                "fn main(console: Console, dir: Dir):\n    print(console, read(dir, \"{bad}\"))\n"
            );
            assert!(interpreter::run_in(&esc, &root).is_err(), "interp must reject `{bad}`");
            let m = parser::parse_module(&esc).expect("parse");
            let w = codegen::compile_module(&m).expect("compile");
            let mut rt = Runtime::batch().expect("runtime");
            let mut a = rt
                .spawn(
                    w.as_bytes(),
                    Capabilities {
                        print: true,
                        quiet: true,
                        dir_root: Some(root.clone()),
                        dir_read: true,
                        dir_write: true,
                        ..Default::default()
                    },
                    64,
                )
                .expect("spawn");
            assert!(a.run().is_err(), "WASM must trap on `{bad}`");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The Net family compiles to capability-gated host imports and agrees with
    /// the interpreter: a client connects to an allowlisted loopback server,
    /// exchanges a line on both backends, and a non-allowlisted address FAILS
    /// on both.
    #[test]
    fn net_capability_compiles_to_wasm_and_confines() {
        use crate::runtime::{Capabilities, Runtime};
        use std::io::{BufRead, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = format!("127.0.0.1:{}", server.local_addr().unwrap().port());
        // One echo exchange per backend run.
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let (stream, _) = server.accept().expect("accept");
                let mut reader = std::io::BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).expect("read");
                let reply = format!("echo: {}\n", line.trim_end());
                reader.get_mut().write_all(reply.as_bytes()).expect("write");
            }
        });

        let src = format!(
            "fn main(console: Console, net: Net):\n    let sock = connect(net, \"{addr}\")\n    send_line(sock, \"hello\")\n    print(console, recv_line(sock))\n    close(sock)\n"
        );
        let want = vec!["echo: hello".to_string()];
        assert_eq!(
            interpreter::run_with(&src, ".", vec![addr.clone()]).expect("interp"),
            want,
            "interpreter"
        );
        let module = parser::parse_module(&src).expect("parse");
        let wat = codegen::compile_module(&module).expect("compile");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                wat.as_bytes(),
                Capabilities {
                    print: true,
                    quiet: true,
                    net_allow: Some(vec![addr.clone()]),
                    net_connect: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");
        handle.join().expect("server thread");

        // A non-allowlisted address fails on BOTH backends.
        let bad = "fn main(console: Console, net: Net):\n    let sock = connect(net, \"127.0.0.1:1\")\n    print(console, \"connected\")\n";
        assert!(
            interpreter::run_with(bad, ".", vec![addr.clone()]).is_err(),
            "interp must reject a non-allowlisted address"
        );
        let m = parser::parse_module(bad).expect("parse");
        let w = codegen::compile_module(&m).expect("compile");
        let mut rt = Runtime::batch().expect("runtime");
        let mut a = rt
            .spawn(
                w.as_bytes(),
                Capabilities {
                    print: true,
                    quiet: true,
                    net_allow: Some(vec![addr]),
                    net_connect: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn");
        assert!(a.run().is_err(), "WASM must trap on a non-allowlisted address");
    }

    /// Net verbs are enforced at the GRANT: a listening module cannot
    /// instantiate under a connect-only grant, and any net import fails with no
    /// grant at all.
    #[test]
    fn net_rights_enforced_at_instantiation() {
        use crate::runtime::{Capabilities, Runtime};
        let listener_src = "fn main(console: Console, net: Net):\n    let l = listen(net, \"127.0.0.1:39999\")\n    print(console, \"listening\")\n";
        let m = parser::parse_module(listener_src).expect("parse");
        let w = codegen::compile_module(&m).expect("compile");
        let mut rt = Runtime::batch().expect("runtime");
        let denied = rt.spawn(
            w.as_bytes(),
            Capabilities {
                print: true,
                quiet: true,
                net_allow: Some(vec!["127.0.0.1:39999".into()]),
                net_connect: true,
                net_listen: false,
                ..Default::default()
            },
            64,
        );
        assert!(denied.is_err(), "listen import must not instantiate under connect-only");
        let client = "fn main(console: Console, net: Net):\n    let s = connect(net, \"127.0.0.1:1\")\n    print(console, \"x\")\n";
        let m = parser::parse_module(client).expect("parse");
        let w = codegen::compile_module(&m).expect("compile");
        let mut rt = Runtime::batch().expect("runtime");
        let denied = rt.spawn(
            w.as_bytes(),
            Capabilities { print: true, quiet: true, ..Default::default() },
            64,
        );
        assert!(denied.is_err(), "net import must not instantiate without a Net grant");
    }

    /// Rights are enforced at the GRANT: a module that imports a write operation
    /// cannot even instantiate under a read-only Dir grant, and any Dir import
    /// fails with no grant at all.
    #[test]
    fn dir_rights_enforced_at_instantiation() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_wasm_dir_rights_{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("mkdir");
        let writer = "fn main(console: Console, dir: Dir):\n    write(dir, \"x.txt\", \"data\")\n    print(console, \"wrote\")\n";
        let module = parser::parse_module(writer).expect("parse");
        let wat = codegen::compile_module(&module).expect("compile");
        let mut rt = Runtime::batch().expect("runtime");
        let denied = rt.spawn(
            wat.as_bytes(),
            Capabilities {
                print: true,
                quiet: true,
                dir_root: Some(root.clone()),
                dir_read: true,
                dir_write: false,
                ..Default::default()
            },
            64,
        );
        assert!(denied.is_err(), "write import must not instantiate under a read-only grant");
        let reader = "fn main(console: Console, dir: Dir):\n    print(console, read(dir, \"x.txt\"))\n";
        let m = parser::parse_module(reader).expect("parse");
        let w = codegen::compile_module(&m).expect("compile");
        let mut rt = Runtime::batch().expect("runtime");
        let denied = rt.spawn(
            w.as_bytes(),
            Capabilities { print: true, quiet: true, ..Default::default() },
            64,
        );
        assert!(denied.is_err(), "Dir import must not instantiate without a Dir grant");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The enforcement half: a module that imports `now`/`env_*` but was NOT
    /// granted Clock/Env must fail at instantiation — the host function simply
    /// is not linked, so the authority is structurally absent.
    #[test]
    fn ungranted_clock_and_env_fail_to_instantiate() {
        use crate::runtime::{Capabilities, Runtime};
        let srcs = [
            "fn main(console: Console, clock: Clock):\n    print(console, to_string(now(clock)))\n",
            "import option\n\nfn main(console: Console, env: Env):\n    match get_env(env, \"X\"):\n        Some(v) -> print(console, v)\n        None -> print(console, \"unset\")\n",
        ];
        for src in srcs {
            let module = parser::parse_module(src).expect("parse");
            let linked = crate::linker::link(vec![("main".into(), module)], "main").expect("link");
            let wat = codegen::compile_module(&linked).expect("compile");
            let mut rt = Runtime::batch().expect("runtime");
            let denied = rt.spawn(
                wat.as_bytes(),
                Capabilities { print: true, ..Default::default() },
                4,
            );
            assert!(denied.is_err(), "ungranted Clock/Env import must fail to instantiate");
        }
    }

    /// Structural `==`/`!=` on compound values (lists, nested lists, tuples,
    /// records, lists of records) must agree on both backends. WASM previously
    /// compared heap POINTERS, so two equal-but-distinct values compared unequal;
    /// codegen now derives the operands' `EqShape` and routes through generated
    /// per-shape structural-equality helpers. (Regression for the silent
    /// compound-`==` pointer-compare divergence.)
    #[test]
    fn structural_equality_agrees_on_both_backends() {
        let src = "type Pt:\n    x: Int\n    y: Int\ntype Bag:\n    items: List(Int)\nfn main(console: Console):\n    print(console, to_string([1, 2, 3] == [1, 2, 3]))\n    print(console, to_string([1, 2, 3] == [1, 9, 3]))\n    print(console, to_string([[1], [2]] == [[1], [2]]))\n    print(console, to_string((1, \"a\") == (1, \"a\")))\n    print(console, to_string((1, \"a\") != (1, \"b\")))\n    print(console, to_string(Pt(1, 2) == Pt(1, 2)))\n    print(console, to_string(Pt(1, 2) == Pt(3, 4)))\n    print(console, to_string([Pt(1, 2)] == [Pt(1, 2)]))\n    print(console, to_string(Bag([1, 2]) == Bag([1, 2])))\n    print(console, to_string([\"a\", \"b\"] == [\"a\", \"b\"]))\n";
        let want = vec![
            "true".to_string(),  // [1,2,3] == [1,2,3]
            "false".to_string(), // [1,2,3] == [1,9,3]
            "true".to_string(),  // nested lists
            "true".to_string(),  // tuple ==
            "true".to_string(),  // tuple != (differs)
            "true".to_string(),  // record ==
            "false".to_string(), // record == (differs)
            "true".to_string(),  // list of records
            "true".to_string(),  // record with a List field
            "true".to_string(),  // list of strings
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// Structural `==` on sum types: nullary enums and concrete-field variants
    /// compare by tag (then by the matched variant's fields) on both backends.
    /// (Regression for the silent ADT pointer-compare divergence.)
    #[test]
    fn adt_structural_equality_agrees_on_both_backends() {
        let src = "type Color:\n    Red\n    Green\n    Blue\ntype Shape:\n    Circle(Int)\n    Square(Int)\nfn main(console: Console):\n    print(console, to_string(Red == Red))\n    print(console, to_string(Red == Blue))\n    print(console, to_string(Circle(3) == Circle(3)))\n    print(console, to_string(Circle(3) == Circle(4)))\n    print(console, to_string(Circle(3) == Square(3)))\n    print(console, to_string([Red, Green] == [Red, Green]))\n";
        let want = vec![
            "true".to_string(),
            "false".to_string(),
            "true".to_string(),
            "false".to_string(),
            "false".to_string(),
            "true".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// Interpolating a record field — `"${p.x}"` (scalar) and `"${p.tags}"`
    /// (compound) — renders on WASM, including inside a custom `Show` impl. A
    /// field access previously resolved to no value type, so `to_string` of it
    /// errored on the compiled backend even though the field's type is known.
    #[test]
    fn record_field_interpolation_renders_on_wasm() {
        let src = "type Post:\n    title: String\n    views: Int\n    tags: List(Int)\nfn main(console: Console):\n    let p = Post(\"hi\", 9, [1, 2, 3])\n    print(console, \"${p.title} (${p.views}): ${p.tags}\")\n";
        assert_eq!(run_on_wasm(src), vec!["hi (9): [1, 2, 3]".to_string()]);
    }

    /// `Option` `==` is structural on both backends: a single-parameter generic
    /// ADT is instantiated at the comparison site from a constructor literal
    /// (sound for both operands — the type checker guarantees they share a
    /// type). Dict `==` compares entries pairwise in insertion order, exactly
    /// like the interpreter. (Closes the former loud-error gaps.)
    #[test]
    fn option_and_dict_equality_agree_on_both_backends() {
        let src = "import option\n\nfn main(console: Console):\n    print(console, to_string(Some(5) == Some(5)))\n    print(console, to_string(Some(5) == Some(6)))\n    print(console, to_string(Some(5) == None))\n    print(console, to_string(None == None))\n    print(console, to_string(Some(\"a\") == Some(\"a\")))\n    print(console, to_string(Some(\"a\") == Some(\"b\")))\n    let a = dict.insert(dict.insert(dict.new(), \"k\", 1), \"j\", 2)\n    let b = dict.insert(dict.insert(dict.new(), \"k\", 1), \"j\", 2)\n    let c = dict.insert(dict.insert(dict.new(), \"k\", 1), \"j\", 9)\n    let rev = dict.insert(dict.insert(dict.new(), \"j\", 2), \"k\", 1)\n    print(console, to_string(a == b))\n    print(console, to_string(a == c))\n    print(console, to_string(a == rev))\n";
        let want = vec![
            "true".to_string(),
            "false".to_string(),
            "false".to_string(),
            "true".to_string(),
            "true".to_string(),
            "false".to_string(),
            "true".to_string(),  // identical insert order + contents
            "false".to_string(), // differing value
            "false".to_string(), // same pairs, different insertion order
        ];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::linker::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let wat = codegen::compile_module(&linked).expect("compile");
        assert_eq!(link_run(src), want.clone(), "interpreter (linked)");
        assert_eq!(crate::run_wat_capture(&wat).expect("wasm"), want, "compiled WASM must agree");
    }

    /// A MULTI-parameter generic ADT (`Result`, whose Ok and Err payloads are
    /// different type variables) is structural on both backends: payloads pin
    /// from constructor literals (the variant's own variables must unify with
    /// its arguments; the other variant's take a safe placeholder), from
    /// declared parameter types, and from declared function returns. (Closes
    /// the last loud equality gap.)
    #[test]
    fn result_equality_agrees_on_both_backends() {
        let src = "import result\n\nfn classify(n: Int) -> Result(Int, String):\n    if n >= 0: Ok(n) else: Err(\"negative\")\n\nfn same(a: Result(Int, String), b: Result(Int, String)) -> Bool:\n    a == b\n\nfn main(console: Console):\n    print(console, to_string(classify(5) == Ok(5)))\n    print(console, to_string(classify(5) == Ok(6)))\n    print(console, to_string(classify(0 - 1) == Err(\"negative\")))\n    print(console, to_string(classify(0 - 1) == Err(\"positive\")))\n    print(console, to_string(classify(5) == Err(\"negative\")))\n    print(console, to_string(same(Ok(1), Ok(1))))\n    print(console, to_string(same(Err(\"a\"), Err(\"a\"))))\n    print(console, to_string(same(Ok(1), Err(\"a\"))))\n    print(console, to_string(Ok([1, 2]) == Ok([1, 2])))\n    print(console, to_string(Ok([1, 2]) == Ok([1, 3])))\n";
        let want: Vec<String> =
            ["true", "false", "true", "false", "false", "true", "true", "false", "true", "false"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// A RECURSIVE generic ADT (`Stack(a)`, whose `Push` carries a `Stack(a)`)
    /// compares structurally on both backends: the shape is identified by its
    /// type arguments and the generated helper calls itself for the
    /// self-referential field — deep spines compare by content, through
    /// literals, declared parameter types, and nullary constructors.
    #[test]
    fn recursive_generic_adt_equality_agrees_on_both_backends() {
        let src = "type Stack:\n    Empty\n    Push(a, Stack(a))\n\nfn same(s: Stack(Int), t: Stack(Int)) -> Bool:\n    s == t\n\nfn main(console: Console):\n    print(console, to_string(Push(2, Push(1, Empty)) == Push(2, Push(1, Empty))))\n    print(console, to_string(Push(2, Push(1, Empty)) == Push(2, Push(9, Empty))))\n    print(console, to_string(Push(\"b\", Push(\"a\", Empty)) == Push(\"b\", Push(\"a\", Empty))))\n    print(console, to_string(Push(\"b\", Push(\"a\", Empty)) == Push(\"b\", Push(\"z\", Empty))))\n    print(console, to_string(same(Push(1, Empty), Push(1, Empty))))\n    print(console, to_string(same(Push(1, Empty), Empty)))\n";
        let want: Vec<String> = ["true", "false", "true", "false", "true", "false"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// The boundary of structural equality stays LOUD: a generic ADT whose
    /// payload is unresolvable at the comparison site (a generic function's
    /// `Result(a, String)` return, with a non-primitive `a` the monomorphizer
    /// can't specialize) is a codegen error — never a silent pointer compare.
    #[test]
    fn unsupported_compound_equality_is_a_loud_error_not_silent() {
        let res = "import result\n\nfn wrap(x: a) -> Result(a, String):\n    Ok(x)\n\nfn main(console: Console):\n    print(console, to_string(wrap([1]) == wrap([2])))\n";
        let rm = parser::parse_module(res).expect("parse");
        let linked = crate::linker::link(vec![("main".into(), rm)], "main").expect("link");
        assert!(
            codegen::compile_module(&linked).is_err(),
            "an unresolvable generic payload must stay a loud codegen error"
        );
    }

    /// Ordering a NaN must FAIL on both backends, not silently return IEEE false
    /// on WASM. The interpreter errors ("cannot compare NaN"); the compiled
    /// `<`/`<=`/`>`/`>=` on floats route through a NaN-trapping helper. Equality
    /// (`==`) is IEEE on both (NaN == NaN is false) and still agrees. Ordinary
    /// float ordering is unchanged. (Regression for a silent NaN-ordering
    /// divergence.)
    #[test]
    fn nan_ordering_errors_on_both_backends() {
        for cmp in ["nan < 1.0", "nan > 1.0", "nan <= nan", "nan >= 0.0"] {
            let src = format!(
                "fn main(console: Console):\n    let nan = 0.0 / 0.0\n    print(console, to_string({cmp}))\n"
            );
            let module = parser::parse_module(&src).expect("parse");
            let wat = codegen::compile_module(&module).expect("compile");
            assert!(interpreter::run(&src).is_err(), "interpreter must error on `{cmp}`");
            assert!(crate::run_wat_capture(&wat).is_err(), "WASM must trap on `{cmp}`");
        }
        // Ordinary float ordering and NaN equality still agree.
        let ok = "fn main(console: Console):\n    let nan = 0.0 / 0.0\n    print(console, to_string(1.5 < 2.5))\n    print(console, to_string(2.5 <= 2.5))\n    print(console, to_string(nan == nan))\n";
        let want = vec!["true".to_string(), "true".to_string(), "false".to_string()];
        assert_eq!(interp(ok), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(ok), want, "compiled WASM must agree");
    }

    /// `string_to_int` of a value that overflows i64 must FAIL on both backends,
    /// not silently wrap on WASM. The compiled `$str_to_int` now traps once the
    /// running magnitude would exceed the sign-appropriate i64 bound (2^63-1, or
    /// 2^63 for a negative), matching Rust's checked parse. The exact boundaries
    /// (i64::MAX / i64::MIN) still parse. (Regression for a silent overflow-wrap
    /// divergence.)
    #[test]
    fn string_to_int_overflow_errors_on_both_backends() {
        let err_cases = [
            "99999999999999999999999",
            "9223372036854775808",  // i64::MAX + 1
            "-9223372036854775809", // i64::MIN - 1
        ];
        for v in err_cases {
            let src = format!(
                "fn main(console: Console):\n    print(console, to_string(string.to_int(\"{v}\")))\n"
            );
            let module = parser::parse_module(&src).expect("parse");
            let wat = codegen::compile_module(&module).expect("compile");
            assert!(interpreter::run(&src).is_err(), "interpreter must error on `{v}`");
            assert!(crate::run_wat_capture(&wat).is_err(), "WASM must trap on `{v}`");
        }
        // The exact i64 boundaries parse identically on both backends.
        let ok = "fn main(console: Console):\n    print(console, to_string(string.to_int(\"9223372036854775807\")))\n    print(console, to_string(string.to_int(\"-9223372036854775808\")))\n";
        let want = vec![
            "9223372036854775807".to_string(),
            "-9223372036854775808".to_string(),
        ];
        assert_eq!(interp(ok), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(ok), want, "compiled WASM must agree");
    }

    /// Indexing a list out of bounds must FAIL on both backends, not silently
    /// read adjacent heap on WASM. The compiled `$list_at` bounds-checks and traps
    /// (like division-by-zero), matching the interpreter's "index out of bounds"
    /// error. In-bounds indexing still agrees. (Regression for a silent OOB-read
    /// divergence.)
    #[test]
    fn list_index_out_of_bounds_errors_on_both_backends() {
        let oob = "fn main(console: Console):\n    let xs = [1, 2, 3]\n    print(console, to_string(list.at(xs, 5)))\n";
        let module = parser::parse_module(oob).expect("parse");
        let wat = codegen::compile_module(&module).expect("compile");
        assert!(interpreter::run(oob).is_err(), "interpreter must error on OOB index");
        assert!(crate::run_wat_capture(&wat).is_err(), "WASM must trap on OOB index");
        // A negative index likewise traps (it used to read backwards into the heap).
        let neg = "fn main(console: Console):\n    let xs = [1, 2, 3]\n    print(console, to_string(list.at(xs, 0 - 1)))\n";
        let nmod = parser::parse_module(neg).expect("parse");
        let nwat = codegen::compile_module(&nmod).expect("compile");
        assert!(interpreter::run(neg).is_err(), "interpreter must error on negative index");
        assert!(crate::run_wat_capture(&nwat).is_err(), "WASM must trap on negative index");
        // In-bounds indexing still agrees.
        let ok = "fn main(console: Console):\n    let xs = [10, 20, 30]\n    print(console, to_string(list.at(xs, 2)))\n";
        assert_eq!(interp(ok), vec!["30".to_string()], "interpreter");
        assert_eq!(run_on_wasm(ok), vec!["30".to_string()], "compiled WASM must agree");
    }

    /// `trim` must strip exactly the same whitespace on both backends. The WASM
    /// `$is_ws` helper handles the 6 ASCII whitespace bytes (incl. VT/FF); Rust's
    /// `str::trim` would also strip Unicode whitespace (e.g. NBSP), which WASM does
    /// not — so the interpreter is pinned to the ASCII set. Here a NBSP (U+00A0)
    /// must survive on BOTH backends, while VT/FF are stripped by both. (Regression
    /// for a silent Unicode-whitespace trim divergence.)
    #[test]
    fn trim_whitespace_set_agrees_on_both_backends() {
        // "  \t\n hi \r\x0b" -> "hi"; "\x0c x \x0c" -> "x"; NBSP stays around 'y'.
        let src = "fn main(console: Console):\n    print(console, \"[\" <> string.trim(\"  \\t\\n hi \\r\u{0b}\") <> \"]\")\n    print(console, \"[\" <> string.trim(\"\u{0c} x \u{0c}\") <> \"]\")\n    print(console, \"[\" <> string.trim(\"\u{a0}y\u{a0}\") <> \"]\")\n";
        let want = vec![
            "[hi]".to_string(),
            "[x]".to_string(),
            "[\u{a0}y\u{a0}]".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// `to_string` of a builtin call result (`has` -> Bool, `size` -> Int) must
    /// compile and render the same on both backends — codegen knows these
    /// builtins' value types, so it picks the right formatter instead of erroring
    /// with "could not determine the value's type". (Regression for the
    /// call-result val-type gap that previously forced `int_to_string`/explicit
    /// conversion.)
    #[test]
    fn to_string_of_builtin_call_results_agrees() {
        let src = "fn main(console: Console):\n    let d = dict.insert(dict.insert(dict.new(), \"a\", 1), \"b\", 2)\n    print(console, to_string(dict.has(d, \"a\")))\n    print(console, to_string(dict.has(d, \"z\")))\n    print(console, to_string(dict.size(d)))\n    print(console, to_string(string.contains(\"hello\", \"ell\")))\n";
        let want = vec![
            "true".to_string(),
            "false".to_string(),
            "2".to_string(),
            "true".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// An early `return` inside an `inout` function must agree on both backends.
    /// An inout function yields multiple results (the declared return plus one per
    /// inout param), so an early return reproduces that epilogue: it pushes each
    /// inout param's current value before returning. (Regression for the
    /// interpreter-only return-in-inout gap.)
    #[test]
    fn return_in_inout_fn_agrees_on_both_backends() {
        let src = "fn clamp(inout n: Int):\n    if (n > 10):\n        n = 10\n        return\n    n = n + 1\n\nfn main(console: Console):\n    var a = 5\n    clamp(a)\n    print(console, to_string(a))\n    var b = 50\n    clamp(b)\n    print(console, to_string(b))\n";
        let want = vec!["6".to_string(), "10".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// The `?` operator inside an `inout` function must agree on both backends.
    /// `?` early-returns the Err, and (like the interpreter's `Flow::Return`) the
    /// inout param is still written back at its value on the error path — so WASM
    /// pushes the inout params before the `?`-return too. (Regression for the
    /// interpreter-only `?`-in-inout gap.)
    #[test]
    fn try_in_inout_fn_agrees_on_both_backends() {
        let src = "import result\n\nfn step(inout n: Int, r: Result(Int, String)) -> Result(Int, String):\n    n = n + 100\n    let got = r?\n    n = n + got\n    Ok(n)\n\nfn describe(r: Result(Int, String)) -> String:\n    match r:\n        Ok(v) -> \"ok:\" <> to_string(v)\n        Err(e) -> \"err:\" <> e\n\nfn main(console: Console):\n    var a = 1\n    let ok = step(a, Ok(5))\n    print(console, to_string(a))\n    print(console, describe(ok))\n    var b = 1\n    let bad = step(b, Err(\"nope\"))\n    print(console, to_string(b))\n    print(console, describe(bad))\n";
        let want = vec![
            "106".to_string(),
            "ok:106".to_string(),
            "101".to_string(),
            "err:nope".to_string(),
        ];
        // `import result` brings the Result type's Ok/Err constructors into scope
        // for codegen, so link first, then run each backend on the linked module.
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::linker::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let wat = codegen::compile_module(&linked).expect("compile");
        assert_eq!(link_run(src), want.clone(), "interpreter (linked)");
        assert_eq!(crate::run_wat_capture(&wat).expect("wasm run"), want, "compiled WASM must agree");
    }

    /// The `encoding` module (hex/base64) must agree on both backends. WASM
    /// bridges each `String -> String` transform to the same native registry the
    /// interpreter uses (a host import), so output is byte-for-byte identical.
    /// (Regression for the interpreter-only encoding-module gap.)
    #[test]
    fn encoding_module_agrees_on_both_backends() {
        let src = "import encoding\n\nfn main(console: Console):\n    let p = \"Hello, witchy!\"\n    let b = encoding.base64_encode(p)\n    print(console, b)\n    print(console, encoding.base64_decode(b))\n    let h = encoding.hex_encode(p)\n    print(console, h)\n    print(console, encoding.hex_decode(h))\n    print(console, encoding.base64_encode(\"foo\"))\n";
        let want = vec![
            "SGVsbG8sIHdpdGNoeSE=".to_string(),
            "Hello, witchy!".to_string(),
            "48656c6c6f2c2077697463687921".to_string(),
            "Hello, witchy!".to_string(),
            "Zm9v".to_string(),
        ];
        // `import encoding` is a native module: link to register its signatures,
        // then run each backend on the linked module.
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::linker::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let wat = codegen::compile_module(&linked).expect("compile");
        assert_eq!(link_run(src), want.clone(), "interpreter (linked)");
        assert_eq!(crate::run_wat_capture(&wat).expect("wasm run"), want, "compiled WASM must agree");
    }

    /// Dict `update` (single-lookup upsert) must agree on both backends, including
    /// nested updates and a big-`Int` value. WASM lowers it to a `$dict_update`
    /// helper that reads the current value (or default), applies the closure via
    /// `call_indirect`, and reinserts — equivalent to the interpreter's
    /// `dict.insert(d, k, f(dict.get_or(d, k, default)))`. (Regression for the
    /// interpreter-only dict-upsert gap.)
    #[test]
    fn dict_update_upsert_agrees_on_both_backends() {
        let src = "fn main(console: Console):\n    let d = dict.insert(dict.insert(dict.new(), \"a\", 1), \"b\", 2)\n    let d2 = dict.update(d, \"a\", 0, fn(x: Int): x + 10)\n    let d3 = dict.update(d2, \"c\", 100, fn(x: Int): x + 1)\n    print(console, to_string(dict.get_or(d3, \"a\", -1)))\n    print(console, to_string(dict.get_or(d3, \"b\", -1)))\n    print(console, to_string(dict.get_or(d3, \"c\", -1)))\n    print(console, to_string(dict.size(d3)))\n    let counts = dict.update(dict.update(dict.new(), \"hit\", 0, fn(n: Int): n + 1), \"hit\", 0, fn(n: Int): n + 1)\n    print(console, to_string(dict.get_or(counts, \"hit\", -1)))\n";
        let want = vec![
            "11".to_string(),
            "2".to_string(),
            "101".to_string(),
            "3".to_string(),
            "2".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// `to_string` on a `Float` must produce the same text on both backends.
    /// WASM has no float formatter in hand-written WAT, so codegen calls a
    /// `float_to_str` host import that formats with Rust `Display` — byte-for-byte
    /// the interpreter's format. (Regression for the interpreter-only float
    /// `to_string` gap.)
    #[test]
    fn float_to_string_agrees_on_both_backends() {
        let src = "fn main(console: Console):\n    print(console, to_string(3.5))\n    print(console, to_string(2.0))\n    print(console, to_string(0.0 - 1.0 / 3.0))\n    print(console, to_string(0.1 + 0.2))\n    print(console, to_string(1000000.0))\n    print(console, to_string(0.0))\n";
        let want = vec![
            "3.5".to_string(),
            "2".to_string(),
            "-0.3333333333333333".to_string(),
            "0.30000000000000004".to_string(),
            "1000000".to_string(),
            "0".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// A closure RETURNED from a function and bound to a `let` (currying) must
    /// keep a big `Int` result on WASM: the binding records the closure's
    /// call-return kind (from the `-> fn(...) -> RET` declaration), so the later
    /// `f(x)` recovers at i64. (Regression for the let-bound-closure-return gap.)
    #[test]
    fn wasm_big_int_through_curried_closure() {
        let src = "fn make(big: Int) -> fn(Int) -> Int:\n    fn(x: Int): x + big\n\nfn main(console: Console):\n    let f = make(5000000000)\n    print(console, to_string(f(1)))\n";
        let want = vec!["5000000001".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// A big `Int` destructured from a tuple RETURNED by a (monomorphized)
    /// generic function must keep its 64 bits. The tuple slots carry i64; codegen
    /// now tracks a tuple-returning function's slot types so `let (a, b) = f(...)`
    /// (direct or via a `let`) reads each at the right width.
    #[test]
    fn wasm_big_int_from_returned_tuple() {
        let src = "fn pair(x: a, y: a) -> (a, a):\n    (x, y)\n\nfn main(console: Console):\n    let (p, q) = pair(9000000000, 1)\n    print(console, to_string(p))\n    print(console, to_string(q))\n";
        let want = vec!["9000000000".to_string(), "1".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// A `Dict` value (and key) keeps its 64 bits on WASM: the Dict now stores
    /// 16-byte entries with i64 key and i64 value slots, and `get_or` recovers the
    /// value at the default's kind. A big-Int value round-trips; a String value
    /// (a pointer in the low bits) still works. (Regression for big-Int-Dict.)
    #[test]
    fn wasm_dict_keeps_big_int_values() {
        let big = "fn main(console: Console):\n    var d = dict.new()\n    d = dict.insert(d, \"k\", 9000000000)\n    print(console, to_string(dict.get_or(d, \"k\", 0)))\n";
        assert_eq!(interp(big), vec!["9000000000"], "interpreter");
        assert_eq!(run_on_wasm(big), vec!["9000000000"], "WASM");
        let s = "fn main(console: Console):\n    var d = dict.new()\n    d = dict.insert(d, \"a\", \"hello\")\n    print(console, dict.get_or(d, \"a\", \"none\"))\n";
        assert_eq!(interp(s), vec!["hello"], "interpreter (string value)");
        assert_eq!(run_on_wasm(s), vec!["hello"], "WASM (string value)");
    }

    /// Iterating a `Dict`'s `dict.values()` (or binding the list) must keep big-Int
    /// values 64-bit: codegen tracks the Dict's value type from `insert` and
    /// carries it to `dict.values(d)`, so the loop variable is i64.
    #[test]
    fn wasm_dict_values_iteration_keeps_big_ints() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    d = dict.insert(d, \"k\", 9000000000)\n    var s = 0\n    for v in dict.values(d):\n        s = s + v\n    print(console, to_string(s))\n";
        assert_eq!(interp(src), vec!["9000000000"], "interpreter");
        assert_eq!(run_on_wasm(src), vec!["9000000000"], "WASM");
    }

    /// A big `Int` in a tuple ELEMENT of a list must survive being read back —
    /// `list.at(list_of_tuples, i)` then destructured, and `for t in list_of_tuples`.
    /// Codegen tracks a list's element-tuple slot types (literal or variable) and
    /// applies them to the `at`/loop tuple destructure. (Two-level nesting.)
    #[test]
    fn wasm_big_int_in_list_of_tuples() {
        let direct = "fn main(console: Console):\n    let (a, b) = list.at([(9000000000, 1)], 0)\n    print(console, to_string(a))\n    print(console, to_string(b))\n";
        assert_eq!(interp(direct), vec!["9000000000", "1"], "interpreter (direct)");
        assert_eq!(run_on_wasm(direct), vec!["9000000000", "1"], "WASM (direct)");
        let loop_src = "fn main(console: Console):\n    for t in [(9000000000, 1)]:\n        let (a, b) = t\n        print(console, to_string(a))\n";
        assert_eq!(interp(loop_src), vec!["9000000000"], "interpreter (loop)");
        assert_eq!(run_on_wasm(loop_src), vec!["9000000000"], "WASM (loop)");
    }

    /// A big `Int` in a nested list (`list.at(list.at(xs, i), j)`) must survive. Codegen
    /// tracks a list-of-lists' inner element type so the inner `at` recovers it
    /// as i64. (Two levels of list nesting — e.g. a matrix row/column.)
    #[test]
    fn wasm_big_int_in_nested_list() {
        let src = "fn main(console: Console):\n    let m = [[1, 9000000000], [3, 4]]\n    print(console, to_string(list.at(list.at(m, 0), 1)))\n";
        assert_eq!(interp(src), vec!["9000000000"], "interpreter");
        assert_eq!(run_on_wasm(src), vec!["9000000000"], "WASM");
    }

    /// A generic function over `List((a, b))` (the `zip`/`unzip` shape) must keep
    /// big Ints. Monomorphization resolves `a`/`b` from the argument list's
    /// element tuple, the inner `let (x, y) = p` destructures at i64, and the
    /// `List(a)` return carries the element type. (The deepest nesting case.)
    #[test]
    fn wasm_big_int_through_list_of_tuples_generic() {
        let src = "fn firsts(ps: List((a, b))) -> List(a):\n    var out = []\n    for p in ps:\n        let (x, y) = p\n        out = list.push(out, x)\n    out\n\nfn main(console: Console):\n    print(console, to_string(list.at(firsts([(9000000000, 1)]), 0)))\n";
        assert_eq!(interp(src), vec!["9000000000"], "interpreter");
        assert_eq!(run_on_wasm(src), vec!["9000000000"], "WASM");
    }

    /// A big `Int` at ARBITRARY list-nesting depth must survive — via a chain of
    /// `at`, nested `for` loops, and a nested-list parameter. Codegen tracks a
    /// list's `(depth, scalar)` nesting (literal, variable, or declared type) and
    /// peels one level per `at`/loop, so the scalar is recovered as i64 at any
    /// depth. (Closes the recursive nested-collection class.)
    #[test]
    fn wasm_big_int_at_arbitrary_list_depth() {
        // Depth-4 `at` chain (literal).
        let chain = "fn main(console: Console):\n    let xs = [[[[9000000000]]]]\n    print(console, to_string(list.at(list.at(list.at(list.at(xs, 0), 0), 0), 0)))\n";
        assert_eq!(interp(chain), vec!["9000000000"], "interpreter (at-chain)");
        assert_eq!(run_on_wasm(chain), vec!["9000000000"], "WASM (at-chain)");
        // Depth-3 nested loops through a nested-list parameter.
        let loops = "fn total(c: List(List(List(Int)))) -> Int:\n    var s = 0\n    for plane in c:\n        for row in plane:\n            for x in row:\n                s = s + x\n    s\n\nfn main(console: Console):\n    print(console, to_string(total([[[9000000000]]])))\n";
        assert_eq!(interp(loops), vec!["9000000000"], "interpreter (loops/param)");
        assert_eq!(run_on_wasm(loops), vec!["9000000000"], "WASM (loops/param)");
    }

    /// A big `Int` in a tuple at the bottom of NESTED lists (`[[(big, 1)]]`)
    /// survives: the `(depth, bottom)` nesting allows a tuple bottom, so peeling
    /// to the inner list then destructuring the tuple recovers the Int as i64.
    #[test]
    fn wasm_big_int_in_nested_list_of_tuples() {
        let src = "fn main(console: Console):\n    for inner in [[(9000000000, 1)]]:\n        for t in inner:\n            let (a, b) = t\n            print(console, to_string(a))\n";
        assert_eq!(interp(src), vec!["9000000000"], "interpreter");
        assert_eq!(run_on_wasm(src), vec!["9000000000"], "WASM");
    }

    /// `to_upper`/`to_lower` now compile to WASM (ASCII case mapping), matching
    /// the interpreter's ASCII fold byte-for-byte — no longer interpreter-only.
    #[test]
    fn wasm_ascii_case_mapping() {
        let src = "fn main(console: Console):\n    print(console, string.to_upper(\"Hi, World! 9z\"))\n    print(console, string.to_lower(\"Hi, World! 9A\"))\n";
        let want = vec!["HI, WORLD! 9Z".to_string(), "hi, world! 9a".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// A large `Int` carried as an `Option`/`Result` success payload must keep its
    /// 64 bits on WASM, through both `?` and a `match`. The payload field is a type
    /// variable (generic i32 ABI), so codegen would truncate; it now tracks the
    /// declared scalar payload type and recovers `Some`/`Ok` values (and `?`
    /// results) at i64. (Regression for the big-Int-through-Option/Result gap.)
    #[test]
    fn wasm_big_int_through_result_payload_and_try() {
        let src = "type Result:\n    Ok(a)\n    Err(e)\n\nfn fetch() -> Result(Int, String):\n    Ok(5000000000)\n\nfn chain() -> Result(Int, String):\n    let x = (fetch())?\n    Ok((x + 1))\n\nfn main(console: Console):\n    match chain():\n        Ok(v) -> print(console, to_string(v))\n        Err(e) -> print(console, e)\n";
        let want = vec!["5000000001".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// `float_to_int` on a non-finite or out-of-range Float must saturate the same
    /// way on both backends. The interpreter uses Rust's `as i64` (NaN -> 0,
    /// +inf -> i64::MAX, -inf -> i64::MIN, out-of-range clamps); WASM used the
    /// trapping `i64.trunc_f64_s` and would crash on those, so it now uses the
    /// saturating `i64.trunc_sat_f64_s`.
    #[test]
    fn wasm_float_to_int_saturates_like_the_interpreter() {
        let src = "fn main(console: Console):\n    print(console, to_string(math.to_int(1.0 / 0.0)))\n    print(console, to_string(math.to_int(0.0 - 1.0 / 0.0)))\n    print(console, to_string(math.to_int(0.0 / 0.0)))\n    print(console, to_string(math.to_int(0.0 - 3.9)))\n";
        let want = vec![
            "9223372036854775807".to_string(),
            "-9223372036854775808".to_string(),
            "0".to_string(),
            "-3".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// `string_to_int` must accumulate in i64 (matching the interpreter's
    /// `parse::<i64>()`) and trim surrounding whitespace. WASM used to parse into
    /// i32, so a value past 2^31 (e.g. 5000000000) silently truncated to a wrong
    /// number; it now agrees on both backends.
    #[test]
    fn wasm_string_to_int_uses_i64_and_trims() {
        let src = "fn main(console: Console):\n    print(console, to_string(string.to_int(\"5000000000\")))\n    print(console, to_string(string.to_int(\"-7000000000\")))\n    print(console, to_string(string.to_int(\"  42  \")))\n";
        let want = vec![
            "5000000000".to_string(),
            "-7000000000".to_string(),
            "42".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// `examples/rle.witchy` — run-length encoding and its inverse — collapses
    /// runs to "<count><char>" and expands them back, verifying decode∘encode is
    /// the identity. Pure string processing; identical on both backends. (Its
    /// run-counting loop is what exposed the two-`at`-results comparison gap.)
    #[test]
    fn rle_example_round_trips_runs() {
        assert_eq!(
            crate::execute_file("examples/rle.witchy", Vec::new()).unwrap(),
            vec![
                "\"aaabbbbc\" -> \"3a4b1c\"  roundtrip ok",
                "\"wwwwww\" -> \"6w\"  roundtrip ok",
                "\"abcdef\" -> \"1a1b1c1d1e1f\"  roundtrip ok",
                "\"mississippi\" -> \"1m1i2s1i2s1i2p1i\"  roundtrip ok",
                "\"\" -> \"\"  roundtrip ok",
            ]
        );
    }

    /// `std/time` computes the civil UTC date from a unix timestamp (Hinnant's
    /// days<->civil algorithm), cross-checked against Python's datetime: leap
    /// years, weekday, an exact round-trip, and a pre-1970 timestamp (floor
    /// division, so the day is right).
    #[test]
    fn time_module_civil_date_from_unix() {
        let src = r#"import time

fn main(console: Console):
    print(console, time.iso8601(time.from_unix(1780000000)))
    print(console, time.weekday_name(time.from_unix(0)) <> " " <> time.iso8601(time.from_unix(0)))
    print(console, time.iso8601(time.from_unix(-86401)))
    print(console, yn(time.is_leap(2000)) <> yn(time.is_leap(1900)) <> yn(time.is_leap(2024)))
    print(console, yn(time.to_unix(time.from_unix(1780000000)) == 1780000000))

fn yn(b: Bool) -> String:
    if b: "y" else: "n"
"#;
        assert_eq!(
            link_run(src),
            vec![
                "2026-05-28T20:26:40Z",
                "Thursday 1970-01-01T00:00:00Z",
                "1969-12-30T23:59:59Z",
                "yny",
                "y",
            ]
        );
    }

    /// `std/csv` round-trips RFC-4180-ish CSV: quoted fields with embedded commas,
    /// doubled quotes (`""`), proper re-quoting on encode, and header records.
    #[test]
    fn csv_module_parses_quotes_and_encodes() {
        let src = r#"import csv
import string

fn main(console: Console):
    let text = "name,city\nAda,\"London, UK\"\nGrace,\"NY\"\"C\"\"\"\n"
    let rows = csv.parse(text)
    print(console, to_string(list.length(rows)))
    print(console, list.at(list.at(rows, 1), 1))
    print(console, list.at(list.at(rows, 2), 1))
    let enc = csv.encode([["a", "b,c"], ["d\"e", "f"]])
    print(console, bs(enc == "a,\"b,c\"\n\"d\"\"e\",f\n"))
    print(console, bs(csv.encode(csv.parse(enc)) == enc))
    let recs = csv.parse_records(text)
    print(console, to_string(list.length(recs)) <> ":" <> dict.get_or(list.at(recs, 0), "city", "?"))

fn bs(b: Bool) -> String:
    if b: "y" else: "n"
"#;
        assert_eq!(
            link_run(src),
            vec!["3", "London, UK", "NY\"C\"", "y", "y", "2:London, UK"]
        );
    }

    /// `std/dict` adds the compositional layer over the builtin Dict: a `get`
    /// returning `Option`, `from_pairs`, and the `map_values`/`filter`/`merge`
    /// transforms — verified against the builtin `size`/`get_or`.
    #[test]
    fn dict_module_higher_level_operations() {
        let src = r#"import dict
import string

fn main(console: Console):
    let d = dict.from_pairs([("a", 1), ("b", 2), ("c", 3)])
    print(console, to_string(dict.size(d)))
    print(console, oi(dict.get(d, "b")))
    print(console, oi(dict.get(d, "z")))
    let m = dict.merge(d, dict.from_pairs([("b", 20), ("d", 4)]))
    print(console, to_string(dict.get_or(m, "b", 0)) <> "," <> to_string(dict.get_or(m, "d", 0)))
    let tens = dict.map_values(d, fn(v: Int): v * 10)
    print(console, oi(dict.get(tens, "c")))
    let evens = dict.filter(d, fn(k: String, v: Int): v % 2 == 0)
    print(console, to_string(dict.size(evens)))
    print(console, bs(dict.is_empty(dict.new())))

fn oi(o: Option(Int)) -> String:
    match o:
        Some(n) -> to_string(n)
        None -> "none"

fn bs(b: Bool) -> String:
    if b: "yes" else: "no"
"#;
        assert_eq!(
            link_run(src),
            vec!["3", "2", "none", "20,4", "30", "1", "yes"]
        );
    }

    /// `std/json` typed field accessors: `get_string`/`get_int`/`get_strings`/
    /// `index_string` compose `get`/`index` with the `as_*` coercions — collapsing
    /// the common "read a typed field" pattern, and yielding `[]` for an absent
    /// string array.
    #[test]
    fn json_module_typed_field_accessors() {
        let src = r#"import json
import string

fn main(console: Console):
    match json.decode("{\"name\":\"acme\",\"n\":7,\"caps\":[\"Net\",\"Console\"],\"arr\":[\"a\",\"b\"]}"):
        Ok(d) ->
            print(console, opt(json.get_string(d, "name")))
            print(console, oi(json.get_int(d, "n")))
            print(console, string.join(json.get_strings(d, "caps"), ","))
            print(console, "[" <> string.join(json.get_strings(d, "absent"), ",") <> "]")
        Err(e) -> print(console, "err")

fn opt(o: Option(String)) -> String:
    match o:
        Some(s) -> s
        None -> "(none)"

fn oi(o: Option(Int)) -> String:
    match o:
        Some(n) -> to_string(n)
        None -> "?"
"#;
        assert_eq!(link_run(src), vec!["acme", "7", "Net,Console", "[]"]);
    }

    /// `std/fs` parent_dir + (with a real Dir) the recursive collect — exercised
    /// here for the pure part to confirm the module's functions resolve on import.
    #[test]
    fn fs_module_parent_dir_resolves() {
        let src = "import fs\nfn main(console: Console):\n    print(console, fs.parent_dir(\"a/b/c\"))\n    print(console, fs.parent_dir(\"top\"))\n";
        assert_eq!(link_run(src), vec!["a/b", ""]);
    }

    /// `std/rights` matches capability strings rights-precisely (the logic the pm
    /// check/gate and coven's publish enforcement share): a bare kind covers any
    /// rights of that kind, a bracketed one only a subset — so `Net[Connect]` does
    /// NOT cover full `Net`.
    #[test]
    fn rights_module_covers_capabilities_rights_precisely() {
        let src = r#"import rights
import string

fn main(console: Console):
    print(console, yes(rights.covers("Net", "Net[Listen]")))
    print(console, yes(rights.covers("Net[Connect]", "Net")))
    print(console, yes(rights.covers("Net[Connect, Tcp]", "Net[Connect]")))
    print(console, yes(rights.covers("Dir", "Console")))
    print(console, yes(rights.covered(["Console", "Dir[Read]"], "Dir[Read]")))
    print(console, string.join(rights.uncovered(["Net[Connect]"], ["Net", "Console"]), "|"))

fn yes(b: Bool) -> String:
    if b: "y" else: "n"
"#;
        assert_eq!(
            link_run(src),
            vec!["y", "n", "y", "n", "y", "Net|Console"]
        );
    }

    /// The `Clock` capability yields wall-clock time (ms since epoch) via `now`.
    /// Reading the clock is ambient nondeterminism, so it's capability-gated and
    /// surfaces in the footprint — not a pure builtin.
    #[test]
    fn clock_capability_yields_wall_clock_time() {
        let out = interp(
            "fn main(console: Console, clock: Clock):\n    print(console, to_string(now(clock)))\n",
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
    print(console, to_string(acc))
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
    print(console, to_string(((q).x * (q).y)))
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
    print(console, to_string(sum([1, 2, 3, 4, 5])))
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
    print(console, to_string((area(Circle(5)) + area(Rect(3, 4)))))
"#,
            ),
            (
                "capturing closures + higher-order",
                r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main(console: Console):
    let k = 100
    print(console, to_string(apply(fn(n: Int): (n + k), 5)))
"#,
            ),
            (
                "dicts",
                r#"
fn main(console: Console):
    var d = dict.new()
    d = dict.insert(d, "a", 1)
    d = dict.insert(d, "b", 2)
    d = dict.insert(d, "a", 9)
    print(console, to_string((dict.get_or(d, "a", 0) + dict.size(d))))
"#,
            ),
            (
                "strings",
                r#"
fn main(console: Console):
    print(console, string.replace("a,b,c", ",", "-"))
    print(console, to_string(string.index_of("hello", "l")))
    print(console, string.substring("hello", 1, 4))
    for w in string.split("the cat sat", " "):
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
    let words = string.split("apple banana apple cherry apple", " ")
    print(console, to_string(count_matches(words, "apple")))
"#,
            ),
            (
                "string equality + ordering",
                r#"
fn main(console: Console):
    let a = string.substring("xapple", 1, 6)
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
    print(console, ((to_string(list.at(rev, 0)) <> ",") <> to_string(list.at(rev, 5))))
    print(console, ((to_string(list.length(list.take(xs, 3))) <> ":") <> to_string(list.at(list.take(xs, 3), 2))))
    print(console, to_string(list.at(list.drop(xs, 4), 0)))
    let sorted = list.sort_by(xs, fn(a: Int, b: Int): (a < b))
    print(console, ((to_string(list.at(sorted, 0)) <> "..") <> to_string(list.at(sorted, 5))))
    let pairs = list.zip([1, 2, 3], [10, 20, 30])
    let (pa, pb) = list.at(pairs, 1)
    print(console, to_string((pa + pb)))
    let en = list.enumerate([100, 200])
    let (ei, ev) = list.at(en, 1)
    print(console, to_string(((ei * 1000) + ev)))
    let doubled = list.map(xs, fn(n: Int): (n * 2))
    let evens = list.filter(xs, fn(n: Int): ((n % 2) == 0))
    print(console, to_string(list.fold(doubled, 0, fn(a: Int, b: Int): (a + b))))
    print(console, to_string(list.length(evens)))
    print(console, to_string(list.index_of(xs, 8)))
    print(console, to_string(list.contains(xs, 9)))
    print(console, to_string(list.any(xs, fn(n: Int): (n > 8))))
    print(console, to_string(list.all(xs, fn(n: Int): (n > 0))))
    print(console, to_string(list.sum(xs)))
    print(console, to_string(list.is_empty(xs)))
    print(console, to_string(list.is_empty(list.filter(xs, fn(n: Int): (n > 100)))))
    print(console, to_string(list.count(xs, fn(n: Int): ((n % 2) == 0))))
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
    print(console, to_string(unwrap(Wrap(42), 0)))
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
    print(console, to_string(result.unwrap_or(add_two(3, 4), 0)))
    print(console, to_string(result.unwrap_or(add_two(3, (0 - 1)), 0)))
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
    print(console, to_string(option.unwrap_or(first_even(4, 6), 0)))
    print(console, to_string(option.unwrap_or(first_even(4, 7), 0)))
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
    print(console, to_string(list.find_index(xs, fn(n: Int): (n > 5))))
    print(console, to_string(list.find_index(xs, fn(n: Int): (n > 100))))
    print(console, to_string(list.find_index(xs, fn(n: Int): (n == 1))))
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
    print(console, to_string(list.length(sums)))
    print(console, to_string(list.sum(sums)))
    let spaced = list.intersperse([5, 6, 7], 0)
    print(console, to_string(list.length(spaced)))
    print(console, to_string(list.sum(spaced)))
    print(console, to_string(list.length(list.intersperse([9], 0))))
    print(console, to_string(list.length(list.intersperse([], 0))))
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
    print(console, to_string(list.sum(list.take_while(xs, fn(n: Int): (n < 5)))))
    print(console, to_string(list.sum(list.drop_while(xs, fn(n: Int): (n < 5)))))
    let threes = list.repeat(7, 3)
    print(console, to_string(list.sum(threes)))
    print(console, to_string(list.length(threes)))
    print(console, to_string(list.length(list.repeat(9, 0))))
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
    print(console, to_string(list.length(flat)))
    print(console, to_string(list.sum(flat)))
    let fm = list.flat_map([1, 2, 3], fn(n: Int): [n, (n * 10)])
    print(console, to_string(list.length(fm)))
    print(console, to_string(list.sum(fm)))
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
    print(console, to_string(option.unwrap_or_else(Some(5), fn(): 0)))
    let fallback = 99
    print(console, to_string(option.unwrap_or_else(option.filter(Some(3), fn(n: Int): (n > 10)), fn(): fallback)))
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
    print(console, to_string(result.unwrap_or_else(checked(7), fn(): 0)))
    print(console, to_string(result.unwrap_or_else(checked((0 - 1)), fn(): 42)))
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
    print(console, to_string(option.unwrap_or(chained, 0)))
    let kept = option.filter(s, fn(n: Int): (n > 0))
    print(console, to_string(option.unwrap_or(kept, 0)))
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
    print(console, to_string(option.unwrap_or(option.flatten(nested(7)), (0 - 1))))
    print(console, to_string(option.unwrap_or(option.flatten(nested(0)), (0 - 1))))
    match option.zip(Some(3), Some(4)):
        Some(pair) ->
            let (x, y) = pair
            print(console, to_string((x + y)))
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
    print(console, to_string(option.unwrap_or(option.or(Some(5), Some(9)), 0)))
    print(console, to_string(option.unwrap_or(option.or(None, Some(9)), 0)))
    print(console, to_string(option.unwrap_or(option.or_else(None, fn(): Some(7)), 0)))
    print(console, to_string(option.unwrap_or(option.or_else(Some(3), fn(): Some(7)), 0)))
    print(console, to_string(option.map_or(Some(10), 0, fn(x: Int): (x * 2))))
    print(console, to_string(option.map_or(None, 99, fn(x: Int): (x * 2))))
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
    while (i < string.char_count(s)):
        acc = (acc <> string.substring(s, i, (i + 1)))
        i = (i + 1)
    acc

fn main(console: Console):
    let words = [build("apple"), build("banana")]
    print(console, to_string(eq.member(words, build("banana"))))
    print(console, to_string(eq.member(words, build("cherry"))))
    print(console, to_string(eq.index_of([10, 20, 30], 20)))
    print(console, to_string(eq.index_of([10, 20, 30], 99)))
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
    while (i < string.char_count(s)):
        acc = (acc <> string.substring(s, i, (i + 1)))
        i = (i + 1)
    acc

fn main(console: Console):
    let words = [build("a"), build("b"), build("a"), build("c"), build("b"), build("a")]
    print(console, to_string(eq.count(words, build("a"))))
    print(console, to_string(eq.count(words, build("z"))))
    print(console, string.join(eq.unique(words), ","))
    print(console, to_string(list.length(eq.unique([Tag(1), Tag(2), Tag(1), Tag(2), Tag(3)]))))
    print(console, to_string(eq.count([Tag(1), Tag(2), Tag(1)], Tag(1))))
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
    while (i < string.char_count(s)):
        acc = (acc <> string.substring(s, i, (i + 1)))
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
    print(console, to_string(list.length(set.union([Id(1), Id(2), Id(1)], [Id(2), Id(3)]))))
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
    while (i < string.char_count(s)):
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
    print(console, to_string(ascii.to_digit("4")))
    print(console, to_string(ascii.to_digit("z")))
    print(console, to_string(digit_sum("a1b2c3")))
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
            Coord(x, y) -> (((("(" <> to_string(x)) <> ",") <> to_string(y)) <> ")")

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
        let client = "type Cmd:\n    Inc\n    Dec\n\nfn apply(n: Int, c: Cmd) -> Int:\n    match c:\n        Inc ->\n            let m = n + 1\n            m\n        Dec ->\n            n - 1\n\nfn main(console: Console):\n    print(console, to_string(apply(10, Inc)))\n    print(console, to_string(apply(10, Dec)))\n";
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
    print(console, to_string(((q).x + (q).y)))
    let r = Point(x: 5, y: 6, ..p)
    print(console, to_string(((r).x + (r).y)))
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
    print(console, to_string(list.fold(signs, 0, fn(a: Int, b: Int): (a + b))))
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
    print(console, to_string(list.fold(doubled, 0, fn(a: Int, b: Int): (a + b))))
    print(console, to_string(list.length(list.filter(xs, fn(n: Int): ((n % 2) == 0)))))
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
        let client = "type Point:\n    Point(Int, Int)\n\nimpl Point:\n    fn sum(self) -> Int:\n        match self:\n            Point(x, y) -> x + y\n\nfn main(console: Console):\n    print(console, to_string(sum(Point(4, 5))))\n";
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
    print(console, to_string(mag(Point(3, 4))))
    print(console, to_string(mag(Circle(6))))
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

    /// `string_chars` (the O(n) string -> List(String) primitive behind a fast
    /// `to_chars`) agrees across the interpreter and WASM —
    /// including a multi-byte (UTF-8) character. Counted by Unicode scalar.
    #[test]
    fn string_chars_backends_agree() {
        let src = "fn main(console: Console):\n    let cs = string.chars(\"café\")\n    print(console, to_string(list.length(cs)))\n    print(console, list.at(cs, 0))\n    print(console, list.at(cs, 3))\n";
        let expected = vec!["4".to_string(), "c".to_string(), "é".to_string()];
        // Interpreter (source of truth).
        assert_eq!(interpreter::run(src).expect("interp"), expected);
        // WASM.
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm diverged");
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
    string.join(list.map(xs, fn(n: Int): to_string(n)), ",")
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
    string.join(list.map(xs, fn(n: Int): to_string(n)), ",")
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
    print(console, string.join(list.map(roots, fn(n: Int): to_string(n)), ","))
    let flags = list.map([0, 1, 2, 4, 9, 10, 16, 17], fn(n: Int): if math.is_perfect_square(n): "T" else: "F")
    print(console, string.join(flags, ""))
    print(console, to_string(math.isqrt(-5)))
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
        Some(n) -> to_string(n)
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
import result
fn render(s: String) -> String:
    match url.parse(s):
        Ok(u) -> url.format(u)
        Err(_e) -> "no parse"
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
import result
fn p(s: String) -> String:
    match url.parse(s):
        Ok(u) -> "ok:" <> to_string(url.port(u))
        Err(_e) -> "none"
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
                Some(n) -> "int:" <> to_string(n)
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
    fn json_floats_decode_and_round_trip_on_both_backends() {
        // JSON numbers with fractions/exponents decode to JsonFloat and
        // re-encode through the shared float formatter — identical on both
        // backends (the learning log's F12).
        let client = r#"
import json
fn round_trip(s: String) -> String:
    match json.decode(s):
        Ok(j) -> json.encode(j)
        Err(e) -> "err:" <> e
fn main(console: Console):
    print(console, round_trip("10"))
    print(console, round_trip("-3"))
    print(console, round_trip("3.25"))
    print(console, round_trip("-0.5"))
    print(console, round_trip("1.5e3"))
    print(console, round_trip("{\"pi\": 3.25}"))
"#;
        let want: Vec<String> = ["10", "-3", "3.25", "-0.5", "1500", "{\"pi\":3.25}"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(client), want, "interpreter");
        assert_eq!(wasm_run(client), want, "wasm");
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
    print(console, to_string(string.last_index_of("a.b.c", ".")))
    print(console, to_string(string.last_index_of("nodot", ".")))
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
    string.join(list.map(r, fn(n: Int): to_string(n)), ",")
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
        out = list.push(out, n)
        r = r2
        i = i + 1
    print(console, string.join(list.map(out, fn(n: Int): to_string(n)), ","))
    let (d, _r3) = random.next_below(random.seed(42), 6)
    print(console, to_string(d))
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
        Ok(xs) -> string.join(list.map(xs, fn(n: Int): to_string(n)), ",")
        Err(e) -> "err:" <> e
fn onums(o: Option(List(Int))) -> String:
    match o:
        Some(xs) -> string.join(list.map(xs, fn(n: Int): to_string(n)), ",")
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
    print(console, string.join(list.map(oks, fn(n: Int): to_string(n)), ","))
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
        // a Duration (combined from literals), to_milliseconds bridges back to Int.
        let client = r#"
import duration
fn main(console: Console):
    print(console, to_string(duration.to_milliseconds(duration.from_clock(1, 2, 3))))
    print(console, duration.clock(1h + 2m + 3s))
    print(console, duration.clock(90s))
    print(console, duration.human(1h + 1m + 1s))
    print(console, duration.human(90s))
    print(console, duration.human(5s))
    print(console, duration.human(500ms))
    print(console, to_string(duration.to_milliseconds(duration.hours(2))))
    print(console, to_string(duration.part_minutes(1h + 2m + 3s)))
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
        Some(d) -> to_string(duration.to_milliseconds(d))
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
    string.join(list.map(xs, fn(n: Int): to_string(n)), ",")
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
    print(console, to_string(a + b))
    print(console, to_string(a * 3))
    let big = 9000000000000
    print(console, to_string(big))
    print(console, to_string(big / 1000))
    print(console, to_string(0 - big))
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
    print(console, to_string(list.at(xs, 0)))
    print(console, to_string(list.at(xs, 1)))
    print(console, to_string(list.at(xs, 0) + list.at(xs, 1)))
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
    print(console, to_string(list.length(fs)))
    print(console, to_string(math.to_int(list.at(fs, 1))))
    let pair = (1.5, 9.5)
    let (lo, hi) = pair
    print(console, to_string(math.to_int(hi)))
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
        // `xs[i]` desugars to `list.at(xs, i)`; chained subscripts index nested lists.
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
    fn method_calls_resolve_to_real_methods_only() {
        // Method-call syntax resolves to impl methods (instance + static) and
        // trait-bound dispatch — NOT to arbitrary free functions. A free
        // function called as a method is a loud error naming the spelling.
        let client = r#"
type Counter:
    n: Int

impl Counter:
    fn fresh() -> Counter:
        Counter(0)
    fn bumped(self) -> Counter:
        Counter(self.n + 1)

fn main(console: Console):
    let c = Counter.fresh().bumped().bumped()
    print(console, "${c.n}")
"#;
        let want = vec!["2".to_string()];
        assert_eq!(link_run(client), want, "interpreter");
        assert_eq!(wasm_run(client), want, "wasm");
        // Free-function UFCS is gone — one cut, loud error.
        let ufcs = "fn inc(x: Int) -> Int:\n    x + 1\n\nfn main(console: Console):\n    print(console, \"${5.inc()}\")\n";
        let module = parser::parse_module(ufcs).expect("parse");
        let linked = crate::linker::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked).expect_err("free-fn UFCS must be rejected");
        assert!(
            err.to_string().contains("methods come from `impl` blocks"),
            "got: {err}"
        );
    }

    #[test]
    fn sandbox_runs_compiled_and_captures_output() {
        // `witchy sandbox` compiles to WASM and runs in the capability sandbox,
        // returning the program's output.
        let path = std::env::temp_dir().join("witchy_sandbox_smoke.witchy");
        std::fs::write(
            &path,
            "fn main(console: Console):\n    print(console, to_string(6 * 7))\n",
        )
        .unwrap();
        let (out, exit) =
            crate::run_file_sandboxed(path.to_str().unwrap(), None, Vec::new(), Vec::new(), None)
                .expect("sandbox run");
        assert_eq!(out, vec!["42"]);
        assert_eq!(exit, None, "a Nil-returning main has no exit code");
    }

    /// The sandbox grants exactly the computed footprint: a program combining
    /// argv, Env, and a read-only Dir (minigrep's shape) runs confined, and its
    /// Int-returning `main` becomes the exit code rather than an output line.
    #[test]
    fn sandbox_grants_full_footprint() {
        let root = std::env::temp_dir().join(format!("witchy_sandbox_fp_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("data.txt"), "needle in here\nnothing\n").unwrap();
        let src_path = root.join("prog.witchy");
        std::fs::write(
            &src_path,
            "import option\nimport string\n\nfn main(console: Console, env: Env, dir: Dir[Read], args: List(String)) -> Int:\n    let path = list.at(args, 0)\n    let label = match get_env(env, \"WITCHY_SANDBOX_LABEL\"):\n        Some(v) -> v\n        None -> \"unlabeled\"\n    for line in string.lines(read(dir, path)):\n        if string.contains(line, \"needle\"):\n            print(console, label <> \": \" <> line)\n    0\n",
        )
        .unwrap();
        unsafe { std::env::set_var("WITCHY_SANDBOX_LABEL", "found") };
        let (out, exit) = crate::run_file_sandboxed(
            src_path.to_str().unwrap(),
            Some(root.clone()),
            Vec::new(),
            vec!["data.txt".to_string()],
            None,
        )
        .expect("sandbox run");
        assert_eq!(out, vec!["found: needle in here"]);
        assert_eq!(exit, Some(0), "Int-returning main becomes the exit code");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A build step runs in the **WASM sandbox**: it is compiled, instantiated
    /// with only the BuildOut/BuildRead host functions linked, reads a confined
    /// schema and writes generated source — and a `..` write traps via the same
    /// confinement as a runtime `Dir`.
    #[test]
    fn build_step_runs_in_the_wasm_sandbox() {
        let root = std::env::temp_dir().join(format!("witchy_wasm_build_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let schema = root.join("schema");
        std::fs::create_dir_all(&schema).unwrap();
        std::fs::write(schema.join("svc.txt"), "Greeter").unwrap();

        let module = parser::parse_module(
            "fn build(out: BuildOut, schema: BuildRead):\n    let nl = \"\\n\"\n    write_out(out, \"api.witchy\", \"pub fn service() -> String:\" <> nl <> \"    \\\"\" <> read_build(schema, \"svc.txt\") <> \"\\\"\" <> nl)\n",
        )
        .expect("parse");
        let generated = crate::run_build_step_sandboxed(
            module,
            root.join("out"),
            vec![schema.clone()],
        )
        .expect("sandboxed build step runs");
        assert_eq!(generated, vec!["api.witchy".to_string()]);
        let body = std::fs::read_to_string(root.join("out/api.witchy")).unwrap();
        assert!(body.contains("\"Greeter\""), "generated source embeds the schema value: {body}");

        // A `..` escape traps inside the sandbox, exactly like a runtime Dir.
        let escaper = parser::parse_module(
            "fn build(out: BuildOut):\n    write_out(out, \"../escape.txt\", \"nope\")\n",
        )
        .unwrap();
        let err = crate::run_build_step_sandboxed(escaper, root.join("out2"), Vec::new())
            .expect_err("a `..` write must trap in the sandbox");
        assert!(err.contains("escapes the Dir capability"), "got: {err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A confinement violation in the WASM sandbox surfaces the same clean
    /// message both backends print — the root cause, not
    /// wasmtime's "error while executing at wasm backtrace…" wrapper.
    #[test]
    fn sandbox_confinement_error_is_clean() {
        let root = std::env::temp_dir().join(format!("witchy_sandbox_esc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src_path = root.join("reader.witchy");
        std::fs::write(
            &src_path,
            "fn main(console: Console, dir: Dir[Read], args: List(String)) -> Int:\n    print(console, read(dir, list.at(args, 0)))\n    0\n",
        )
        .unwrap();
        let err = crate::run_file_sandboxed(
            src_path.to_str().unwrap(),
            Some(root.clone()),
            Vec::new(),
            vec!["../secret.txt".to_string()],
            None,
        )
        .expect_err("a `..` traversal must be denied");
        assert!(
            err.contains("escapes the Dir capability"),
            "the denial should cite the Dir capability, got: {err}"
        );
        assert!(
            !err.contains("wasm backtrace"),
            "the raw wasmtime backtrace must not leak to the user, got: {err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn verify_file_agrees_on_a_simple_program() {
        // `witchy verify` runs a program on both backends and confirms identical
        // output; on a normal program that should succeed.
        let path = std::env::temp_dir().join("witchy_verify_smoke.witchy");
        std::fs::write(
            &path,
            "fn main(console: Console):\n    print(console, to_string((2 + 3) * 4))\n    print(console, \"hi\")\n",
        )
        .unwrap();
        crate::verify_file(path.to_str().unwrap()).expect("backends should agree");
    }

    /// `witchy parity` covers ACTOR programs: the compiled side runs the whole
    /// actor system (driver + per-actor VMs via guest spawn) and must agree
    /// with the interpreter line for line.
    #[test]
    fn verify_file_covers_actor_programs() {
        // conventions.witchy is the case whose handler calls a top-level
        // function — the actor module must carry the helper.
        for example in [
            "examples/actors.witchy",
            "examples/dispatch.witchy",
            "examples/conventions.witchy",
        ] {
            crate::verify_file(example).expect("actor program backends should agree");
        }
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
        print(console, to_string(key(it)) <> tag(it))
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
    while (i < string.char_count(s)):
        acc = (acc <> string.substring(s, i, (i + 1)))
        i = (i + 1)
    acc

fn main(console: Console):
    let words = [build("pear"), build("apple"), build("fig"), build("apple")]
    print(console, string.join(ord.sort(words), ","))
    print(console, string.join(ord.sort(["c", "a", "b"]), ""))
    print(console, ord.max_of(build("alpha"), build("omega")))
    print(console, ord.maximum([build("x"), build("a"), build("m")], ""))
    let nums = ord.sort([3, 1, 2, 1])
    print(console, to_string((list.at(nums, 0) + (list.at(nums, 3) * 10))))
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
    print(console, to_string(result.unwrap_or(result.or(checked(5), Ok(9)), 0)))
    print(console, to_string(result.unwrap_or(result.or(checked((0 - 1)), Ok(9)), 0)))
    print(console, to_string(result.unwrap_or(result.or_else(checked((0 - 1)), fn(e: String): Ok(string.length(e))), 0)))
    print(console, to_string(result.map_or(checked(5), 0, fn(x: Int): (x * 2))))
    print(console, to_string(result.map_or(checked((0 - 1)), 99, fn(x: Int): (x * 2))))
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
    print(console, to_string(result.unwrap_or(chained, 0)))
    let mapped = result.map_err(checked((0 - 1)), fn(s: String): string.length(s))
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
                // Mirror the interpreter's automatic grants (output + the
                // read-only ambient Clock/Env), like `run_wat_capture`.
                Capabilities {
                    print: true,
                    print_int: true,
                    clock: true,
                    env: true,
                    dir_root: Some(std::path::PathBuf::from(".")),
                    dir_read: true,
                    dir_write: true,
                    net_allow: Some(Vec::new()),
                    net_connect: true,
                    net_listen: true,
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
                // Mirror the interpreter's automatic grants (output + the
                // read-only ambient Clock/Env), like `run_wat_capture`.
                Capabilities {
                    print: true,
                    print_int: true,
                    clock: true,
                    env: true,
                    dir_root: Some(std::path::PathBuf::from(".")),
                    dir_read: true,
                    dir_write: true,
                    net_allow: Some(Vec::new()),
                    net_connect: true,
                    net_listen: true,
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
    (sum + list.length(evens))
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
    ((list.at(sorted, 0) * 100) + list.at(sorted, 7))
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
        // *and* the Float ones: float_min/float_max/float_abs/float_clamp, which use f64 compares and
        // unary negation). gcd(48,36)=12, pow(2,10)=1024, clamp(15,0,10)=10,
        // float_clamp(15,0,10)=10.0, float_abs(-3.5)=3.5 -> 12+1024+10+10+3 = 1059.
        let client = r#"
import math

fn main() -> Int:
    let a = math.gcd(48, 36)
    let b = math.pow(2, 10)
    let c = math.clamp(15, 0, 10)
    let f = math.float_clamp(15.0, 0.0, 10.0)
    let g = math.float_abs((0.0 - 3.5))
    ((((a + b) + c) + math.to_int(f)) + math.to_int(g))
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
    // A broader compiled-float workout: division, float_abs (negation + compare),
    // float_max, a float comparison driving a float-valued `if`, multiply, subtract,
    // and sqrt — all feeding one Float result. Both backends agree.
    #[test]
    fn float_arithmetic_compiled_backends_agree() {
        let client = r#"
import math

fn main() -> Float:
    let a = (10.0 / 4.0)
    let b = math.float_abs((0.0 - 1.5))
    let c = math.float_max(a, b)
    let d = if (c > 2.0): (c * 2.0) else: 0.0
    (d - math.sqrt(4.0))
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
    (math.float_min(2.5, math.sqrt(2.25)) + math.float_clamp(5.0, 0.0, 1.0))
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
    print(console, to_string(math.factorial(5)))
    print(console, to_string(math.factorial(0)))
    print(console, to_string(math.factorial(1)))
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
    print(console, to_string(math.lcm(4, 6)))
    print(console, to_string(math.lcm(21, 6)))
    print(console, to_string(math.lcm(0, 5)))
    print(console, to_string(math.lcm((0 - 4), 6)))
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
    let p = string.split("a,bb,ccc", ",")
    print(console, to_string(list.length(p)))
    print(console, list.at(p, 0))
    print(console, list.at(p, 2))
    print(console, to_string(list.length(string.split("a,,b", ","))))
    print(console, list.at(string.split("a,,b", ","), 1))
    print(console, to_string(list.length(string.split("", ","))))
    print(console, to_string(list.length(string.split("abc", ""))))
    print(console, list.at(string.split("xXXyXXz", "XX"), 2))
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
    fn to_string_on_compound_renders_on_wasm() {
        // A compound (list/tuple/record/ADT/dict, any nesting) renders byte-
        // identically to the interpreter via a generated per-shape helper — so
        // `to_string`/`${...}` work on WASM, not just the interpreter.
        let src = r#"
type Shape:
    Circle(Int)
    Dot

fn main(console: Console):
    print(console, to_string([1, 2, 3]))
    print(console, "${[[1, 2], [3]]}")
    print(console, "${(1, "two", true)}")
    print(console, "${[Circle(2), Dot]}")
    let d = dict.insert(dict.insert(dict.new(), "a", 1), "b", 2)
    print(console, "${d}")
    let tc = ([1, 2], (3, 4))          // a let-bound tuple whose slots are compound
    print(console, "${tc}")
"#;
        assert_eq!(
            run_on_wasm(src),
            vec![
                "[1, 2, 3]",
                "[[1, 2], [3]]",
                "(1, two, true)",
                "[Circle(2), Dot]",
                "{a: 1, b: 2}",
                "([1, 2], (3, 4))",
            ]
        );
    }

    /// `==` on a compound whose *slots are themselves compound* must agree on
    /// both backends, whether the operands are `let`-bound or parameters. WASM
    /// previously returned `None` for the shape of such a binding and fell back to
    /// a pointer compare — a SILENT divergence (interpreter `true`, compiled
    /// `false`). The shape is now captured from the binding/declared type.
    #[test]
    fn nested_compound_equality_agrees_on_both_backends() {
        let src = "fn same(a: (List(Int), List(Int)), b: (List(Int), List(Int))) -> Bool:\n    a == b\nfn main(console: Console):\n    let v = ([1, 2], (3, 4))\n    let w = ([1, 2], (3, 4))\n    print(console, to_string(v == w))\n    print(console, to_string(same(([1], [2]), ([1], [2]))))\n    print(console, to_string(same(([1], [2]), ([1], [9]))))\n";
        let want = vec!["true".to_string(), "true".to_string(), "false".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    #[test]
    fn to_string_through_generics_renders() {
        // Typed lowering (Phase 0) resolves what used to be undetermined: a
        // generic tuple rendered through a monomorphizable call works
        // identically on both backends. (The loud could-not-determine error
        // remains for shapes with NO resolvable call site.)
        let src = r#"
fn render(t: (a, a)) -> String:
    to_string(t)

fn main(console: Console):
    print(console, render((1, 2)))
"#;
        assert_eq!(link_run(src), vec!["(1, 2)"], "interpreter");
        assert_eq!(wasm_run(src), vec!["(1, 2)"], "wasm");
    }

    #[test]
    fn negative_int_to_string_on_wasm() {
        // `int_to_string` renders negatives with a leading '-' (previously it
        // emitted garbage, e.g. "/" for -1).
        let src = r#"
fn main(console: Console):
    print(console, to_string((0 - 1)))
    print(console, to_string((0 - 128)))
    print(console, to_string(255))
    print(console, to_string(0))
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
    print(console, string.replace("a,b,c", ",", ";"))
    print(console, string.replace("aXXbXXc", "XX", "-"))
    print(console, string.replace("aaa", "aa", "x"))
    print(console, string.replace("a,b,c", ",", ""))
    print(console, string.replace("abc", "b", "XYZ"))
    print(console, string.replace("abc", "z", "Q"))
    print(console, string.replace("ab", "", "-"))
    print(console, string.replace("café", "é", "e"))
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
        // 4 (byte 5), and string.substring(3,5) is the two characters "é!".
        let src = r#"
fn main(console: Console):
    print(console, to_string(if string.contains("hello world", "world"): 1 else: 0))
    print(console, to_string(if string.contains("abc", "xyz"): 1 else: 0))
    print(console, to_string(if string.contains("abc", ""): 1 else: 0))
    print(console, to_string(string.index_of("hello", "l")))
    print(console, to_string(string.index_of("hello", "z")))
    print(console, string.substring("hello", 1, 4))
    print(console, string.substring("hi", 0, 100))
    print(console, string.substring("hi", 5, 10))
    print(console, to_string(string.index_of("café!", "!")))
    print(console, string.substring("café!", 3, 5))
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
    var d = dict.new()
    d = dict.insert(d, "a", 1)
    d = dict.insert(d, "b", 2)
    d = dict.insert(d, "a", 10)
    print(console, to_string(dict.get_or(d, "a", 0)))
    print(console, to_string(dict.get_or(d, "b", 0)))
    print(console, to_string(dict.get_or(d, "z", (0 - 1))))
    print(console, to_string(dict.size(d)))
    print(console, to_string(if dict.has(d, "b"): 1 else: 0))
    print(console, to_string(if dict.has(d, "q"): 1 else: 0))
"#;
        assert_eq!(run_on_wasm(src), vec!["10", "2", "-1", "2", "1", "0"]);
    }

    #[test]
    fn dict_int_keys_on_wasm() {
        // Int-keyed Dict: keys compared with i32 equality (mode 0).
        let src = r#"
fn main(console: Console):
    var d = dict.new()
    d = dict.insert(d, 1, 100)
    d = dict.insert(d, 2, 200)
    print(console, to_string(dict.get_or(d, 1, 0)))
    print(console, to_string(dict.get_or(d, 2, 0)))
    print(console, to_string(dict.get_or(d, 3, (0 - 1))))
"#;
        assert_eq!(run_on_wasm(src), vec!["100", "200", "-1"]);
    }

    #[test]
    fn wordcount_example_runs_on_wasm() {
        // The word-frequency example compiles to WASM: a String-keyed Dict built
        // in a `for w in string.split(...)` loop (so `w`'s type resolves to String).
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
    var d = dict.new()
    d = dict.insert(d, [1, 2], 5)
    print(console, to_string(dict.size(d)))
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
    print(console, to_string((Item(7, 6)).price))
    print(console, to_string((lookup(true)).qty))
    let items = [Item(1, 2), Item(3, 4)]
    print(console, to_string((list.at(items, 1)).qty))
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
    print(console, to_string(pick(true)))
    print(console, to_string(pick(false)))
    print(console, to_string(from_tag(0)))
    print(console, to_string(from_tag(9)))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["30", "10", "2", "5"]);
    }

    #[test]
    fn list_of_records_index_access_backends_agree() {
        // `list.at(items, i).field` via a let, for both a List(Record) parameter and a
        // let-bound list literal of records; and a for-loop over the let-bound
        // list. Both backends agree.
        let src = r#"
type Item:
    price: Int
    qty: Int

fn first_value(items: List(Item)) -> Int:
    let first = list.at(items, 0)
    ((first).price * (first).qty)

fn main(console: Console):
    print(console, to_string(first_value([Item(3, 10), Item(5, 2)])))
    let items = [Item(2, 4), Item(7, 1)]
    let second = list.at(items, 1)
    print(console, to_string(((second).price + (second).qty)))
    var total = 0
    for it in items:
        total = (total + (it).price)
    print(console, to_string(total))
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
    var d = dict.new()
    d = dict.insert(d, "apple", Item(3, 10))
    d = dict.insert(d, "bread", Item(2, 5))
    let it = dict.get_or(d, "apple", Item(0, 0))
    print(console, to_string(((it).price * (it).qty)))
    let missing = dict.get_or(d, "milk", Item(0, 0))
    print(console, to_string((missing).price))
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
    var d = dict.new()
    d = dict.insert(d, "a", 1)
    d = dict.insert(d, "b", 2)
    d = dict.insert(d, "c", 3)
    let d2 = dict.remove(d, "b")
    print(console, to_string(dict.size(d2)))
    print(console, to_string(if dict.has(d2, "b"): 1 else: 0))
    print(console, to_string(dict.get_or(d2, "a", 0)))
    print(console, to_string(dict.get_or(d2, "c", 0)))
    let d3 = dict.remove(d, "missing")
    print(console, to_string(dict.size(d3)))
    print(console, to_string(dict.size(d)))
    var nums = dict.new()
    nums = dict.insert(nums, 10, 100)
    nums = dict.insert(nums, 20, 200)
    let nums2 = dict.remove(nums, 10)
    print(console, to_string(dict.size(nums2)))
    print(console, to_string(dict.get_or(nums2, 20, 0)))
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
    var d = dict.new()
    d = dict.insert(d, "a", 10)
    d = dict.insert(d, "b", 20)
    d = dict.insert(d, "c", 30)
    var ksum = 0
    for k in dict.keys(d):
        ksum = (ksum + string.length(k))
    print(console, to_string(ksum))
    var vsum = 0
    for v in dict.values(d):
        vsum = (vsum + v)
    print(console, to_string(vsum))
    var psum = 0
    for entry in dict.pairs(d):
        let (k, v) = entry
        psum = ((psum + string.length(k)) + v)
    print(console, to_string(psum))
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
    (((list.length(parts) * 100) + string.length(joined)) + string.length(r))
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
    print(console, to_string(list.sum(evens)))
    print(console, to_string(list.sum(odds)))
    let pairs = list.zip([10, 20, 30], [1, 2, 3])
    let (a, b) = list.unzip(pairs)
    print(console, to_string(list.sum(a)))
    print(console, to_string(list.sum(b)))
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
        out = list.push(out, i)
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
    string.length(s)
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
        out = list.push(out, (x * 2))
    out

fn main() -> Int:
    let ys = list.concat(double_all([1, 2, 3]), [100])
    (((list.at(ys, 0) + list.at(ys, 1)) + list.at(ys, 2)) + list.at(ys, 3))
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
    if string.starts_with(s, "ht"):
        if string.ends_with(s, "ml"):
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
        print(console, to_string(count))
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
        es = list.push(es, to_string(func.first(p)) <> func.second(p))
    print(console, string.join(es, " "))
    print(console, to_string(iter.count(iter.zip(iter.count_from(1), iter.from_list([0, 0, 0])))))
    print(console, to_string(iter.sum(iter.chain(iter.range(0, 4), iter.range(10, 13)))))
    print(console, to_string(iter.sum(iter.flat_map(iter.range(1, 4), fn(n: Int): iter.from_list([n, n])))))
    iter.for_each(iter.take(iter.count_from(100), 3), fn(n: Int): print(console, to_string(n)))
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
    print(console, to_string(iter.sum(iter.filter(small, fn(s: Int): s % 2 == 1))))
    // first multiple of 7 above 50, from an infinite iterator
    match iter.find(iter.count_from(51), fn(n: Int): n % 7 == 0):
        Some(n) -> print(console, to_string(n))
        None -> print(console, "none")
    // a finite range, doubled and collected
    print(console, to_string(iter.count(iter.range(0, 5))))
    for v in iter.collect(iter.map(iter.range(0, 3), fn(n: Int): n * 10)):
        print(console, to_string(v))
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

    /// `projects/pm` is the package manager itself, written in witchy. `pm audit`
    /// prints the capability footprint a source file demands — the self-hosted
    /// `witchy caps`, dispatched from a real CLI (`args: List(String)`).
    #[test]
    fn pm_audits_a_files_footprint() {
        let (out, code) = crate::execute_file_exit(
            "projects/pm/src/pm.witchy",
            Vec::new(),
            vec!["audit".into(), "examples/data/sample_rune.witchy".into()],
            None,
        )
        .unwrap();
        assert_eq!(
            out,
            vec!["examples/data/sample_rune.witchy demands: Dir[Read], Net[Connect]"]
        );
        assert_eq!(code, 0);
        // pm reads/writes project files, prints, and `add` fetches over the
        // network — Console, Dir, Net. `compiler.*` is a host introspection
        // intrinsic, not a runtime capability.
        let src = std::fs::read_to_string("projects/pm/src/pm.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console, Dir, Net");
    }

    /// `pm guard <old> <new>` is the supply-chain gate: it asks `compiler.diff`
    /// whether the upgrade widens authority and exits 2 on a widening (wireable
    /// into CI). The sample upgrade adds `Listen`, so it BLOCKs.
    #[test]
    fn pm_guard_blocks_a_widening() {
        let (out, code) = crate::execute_file_exit(
            "projects/pm/src/pm.witchy",
            Vec::new(),
            vec![
                "guard".into(),
                "examples/data/sample_rune.witchy".into(),
                "examples/data/sample_rune_v2.witchy".into(),
            ],
            None,
        )
        .unwrap();
        assert_eq!(out, vec!["BLOCK: upgrade widens authority by Net[Listen]"]);
        assert_eq!(code, 2, "a widening must exit 2");
    }

    /// `pm check <dir>` recomputes a rune's footprint from source and fails if the
    /// manifest's `[capabilities]` does not admit it — rights-precisely. The
    /// `leaky` fixture declares only `Console` but its code reads files, so the
    /// undeclared `Dir[Read]` is caught and the gate exits 2.
    #[test]
    fn pm_check_blocks_an_under_declared_rune() {
        let (out, code) = crate::execute_file_exit(
            "projects/pm/src/pm.witchy",
            Vec::new(),
            vec!["check".into(), "projects/pm/tests/fixtures/leaky".into()],
            None,
        )
        .unwrap();
        assert_eq!(
            out,
            vec!["BLOCK: code demands authority not admitted by [capabilities]: Dir[Read]"]
        );
        assert_eq!(code, 2, "an under-declared rune must exit 2");
    }

    /// pm passes its *own* `check`: its manifest declares exactly `Console, Dir`,
    /// which is what the code demands — the package manager is consistent with
    /// itself, proving the self-hosted gate is honest.
    #[test]
    fn pm_passes_its_own_check() {
        let (out, code) = crate::execute_file_exit(
            "projects/pm/src/pm.witchy",
            Vec::new(),
            vec!["check".into(), "projects/pm".into()],
            None,
        )
        .unwrap();
        assert_eq!(out, vec!["OK: declared footprint admits the code, nothing unused"]);
        assert_eq!(code, 0);
    }

    /// `pm new <name>` scaffolds a runnable rune (manifest + src stub) using the
    /// *write* Dir capability, confined to the workspace root. The scaffold is
    /// real: the generated rune both passes its own `check` and runs.
    #[test]
    fn pm_new_scaffolds_a_runnable_rune() {
        let tmp = std::env::temp_dir().join("witchy_pm_new_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let (linked, _stem) = crate::link_file("projects/pm/src/pm.witchy").expect("link");
        typeck::check(&linked).expect("typeck");
        let (out, code) = interpreter::run_module_exit(
            linked,
            &tmp,
            Vec::new(),
            vec!["new".into(), "widget".into()],
            None,
        )
        .expect("run");
        assert_eq!(code, 0);
        assert!(out.iter().any(|l| l.contains("created rune `widget`")));

        let manifest = std::fs::read_to_string(tmp.join("widget/witchy.toml"))
            .expect("manifest was written");
        assert!(manifest.contains("name = \"widget\""));
        assert!(manifest.contains("runtime = [\"Console\"]"));
        let src = std::fs::read_to_string(tmp.join("widget/src/widget.witchy"))
            .expect("src stub was written");
        assert!(src.contains("hello from widget"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `pm deps <dir>` lists a rune's dependencies and their source — read
    /// straight from `[dependencies]`'s inline tables (`toml.table`/`inline_get`).
    #[test]
    fn pm_lists_dependencies() {
        let (out, code) = crate::execute_file_exit(
            "projects/pm/src/pm.witchy",
            Vec::new(),
            vec!["deps".into(), "examples/projects/ledger/ledger".into()],
            None,
        )
        .unwrap();
        assert_eq!(out, vec!["money -> path:../money"]);
        assert_eq!(code, 0);
    }

    /// `pm info <dir>` summarizes a rune: name, version, declared vs. recomputed
    /// footprint. Run on the pm itself — its declared `[capabilities]` exactly
    /// match what the code demands (Console, Dir, Net), the self-consistency the
    /// `check` gate enforces.
    #[test]
    fn pm_info_summarizes_a_rune() {
        let (out, code) = crate::execute_file_exit(
            "projects/pm/src/pm.witchy",
            Vec::new(),
            vec!["info".into(), "projects/pm".into()],
            None,
        )
        .unwrap();
        assert_eq!(
            out,
            vec![
                "name:     pm",
                "version:  0.1.0",
                "declared: Console, Dir, Net",
                "actual:   Console, Dir, Net",
            ]
        );
        assert_eq!(code, 0);
    }

    /// The interop milestone: `pm verify` recomputes each dependency's content
    /// hash and checks it against the *committed, coven-generated* `witchy.lock`.
    /// It passes — the self-hosted pm's hashing is byte-identical to coven's
    /// store, so a witchy-checked lock and a coven-written one agree.
    #[test]
    fn pm_verify_validates_a_coven_generated_lockfile() {
        let (out, code) = crate::execute_file_exit(
            "projects/pm/src/pm.witchy",
            Vec::new(),
            vec!["verify".into(), "examples/projects/ledger/ledger".into()],
            None,
        )
        .unwrap();
        assert_eq!(out, vec!["OK: every locked hash matches the dependency sources"]);
        assert_eq!(code, 0);
    }

    /// `pm lock` pins dependencies by content hash; `pm verify` accepts the result
    /// and then catches a later edit to a dependency's source (the tamper / stale
    /// case) — exiting 2. Run end to end against a freshly scaffolded workspace.
    #[test]
    fn pm_lock_then_verify_detects_tampering() {
        let tmp = std::env::temp_dir().join(format!("witchy_pm_lock_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("app/src")).unwrap();
        std::fs::create_dir_all(tmp.join("lib/src")).unwrap();
        std::fs::write(
            tmp.join("app/witchy.toml"),
            "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"lib\" = { path = \"../lib\" }\n",
        )
        .unwrap();
        std::fs::write(
            tmp.join("app/src/app.witchy"),
            "fn main(console: Console):\n    print(console, \"hi\")\n",
        )
        .unwrap();
        std::fs::write(
            tmp.join("lib/witchy.toml"),
            "[rune]\nname = \"lib\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            tmp.join("lib/src/lib.witchy"),
            "fn f(s: String) -> String:\n    s\n",
        )
        .unwrap();

        let run_pm = |args: Vec<String>| -> (Vec<String>, i32) {
            let (linked, _stem) = crate::link_file("projects/pm/src/pm.witchy").expect("link");
            typeck::check(&linked).expect("typeck");
            interpreter::run_module_exit(linked, &tmp, Vec::new(), args, None).expect("run")
        };

        // lock pins lib by content hash — the hash must match coven's store.
        let (out, code) = run_pm(vec!["lock".into(), "app".into()]);
        assert_eq!(code, 0, "lock failed: {out:?}");
        let lock = std::fs::read_to_string(tmp.join("app/witchy.lock")).unwrap();
        let store_hash = crate::pm::store::RuneSource::read_dir(&tmp.join("lib"))
            .unwrap()
            .hash();
        assert!(
            lock.contains(&store_hash),
            "lockfile {lock:?} must pin the store hash {store_hash}"
        );

        // A fresh lock verifies clean.
        let (out, code) = run_pm(vec!["verify".into(), "app".into()]);
        assert_eq!(out, vec!["OK: every locked hash matches the dependency sources"]);
        assert_eq!(code, 0);

        // Edit lib's source: the pinned hash no longer matches — verify must BLOCK.
        std::fs::write(
            tmp.join("lib/src/lib.witchy"),
            "fn f(s: String) -> String:\n    s <> s\n",
        )
        .unwrap();
        let (out, code) = run_pm(vec!["verify".into(), "app".into()]);
        assert_eq!(out, vec!["BLOCK: lock no longer matches source for: lib"]);
        assert_eq!(code, 2, "a tampered dependency must exit 2");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `pm gate` is the supply-chain gate: after a dependency is locked, an edit
    /// to its source that *widens* its capability footprint is BLOCKed (exit 2),
    /// with the new authority attributed to the rune that introduced it.
    /// Explicitly accepting those caps (like `--allow-cap`) folds them into the
    /// baseline and clears the block.
    #[test]
    fn pm_gate_blocks_a_dependency_that_widens_authority() {
        let tmp = std::env::temp_dir().join(format!("witchy_pm_gate_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("app/src")).unwrap();
        std::fs::create_dir_all(tmp.join("lib/src")).unwrap();
        std::fs::write(
            tmp.join("app/witchy.toml"),
            "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"lib\" = { path = \"../lib\" }\n",
        )
        .unwrap();
        std::fs::write(
            tmp.join("app/src/app.witchy"),
            "fn main(console: Console):\n    print(console, \"hi\")\n",
        )
        .unwrap();
        std::fs::write(
            tmp.join("lib/witchy.toml"),
            "[rune]\nname = \"lib\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        // lib starts pure: no capabilities.
        std::fs::write(
            tmp.join("lib/src/lib.witchy"),
            "fn f(s: String) -> String:\n    s\n",
        )
        .unwrap();

        let run_pm = |args: Vec<String>| -> (Vec<String>, i32) {
            let (linked, _stem) = crate::link_file("projects/pm/src/pm.witchy").expect("link");
            typeck::check(&linked).expect("typeck");
            interpreter::run_module_exit(linked, &tmp, Vec::new(), args, None).expect("run")
        };

        run_pm(vec!["lock".into(), "app".into()]);
        let (out, code) = run_pm(vec!["gate".into(), "app".into()]);
        assert_eq!(out, vec!["OK: dependencies demand no authority beyond witchy.lock"]);
        assert_eq!(code, 0);

        // lib's source widens to demand Console + Net — gate must BLOCK and name lib.
        std::fs::write(
            tmp.join("lib/src/lib.witchy"),
            "fn main(console: Console, net: Net):\n    let s = connect(net, \"example.com:80\")\n    print(console, \"connected\")\n",
        )
        .unwrap();
        let (out, code) = run_pm(vec!["gate".into(), "app".into()]);
        assert_eq!(
            out,
            vec![
                "BLOCK: dependencies demand new authority: Console, Net",
                "  Console <- lib",
                "  Net <- lib",
            ]
        );
        assert_eq!(code, 2, "a widening dependency must exit 2");

        // Accepting both new caps clears the gate.
        let (out, code) = run_pm(vec![
            "gate".into(),
            "app".into(),
            "Console".into(),
            "Net".into(),
        ]);
        assert_eq!(out, vec!["OK: dependencies demand no authority beyond witchy.lock"]);
        assert_eq!(code, 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The pm hashes a rune with a *nested* `src/` tree (`src/sub/extra.witchy`)
    /// to the same content address as the Rust store — proving its recursive
    /// walk (via the `is_dir` builtin) matches `RuneSource::read_dir`.
    #[test]
    fn pm_lock_hashes_nested_src_like_the_store() {
        let tmp = std::env::temp_dir().join(format!("witchy_pm_nested_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("app/src")).unwrap();
        std::fs::create_dir_all(tmp.join("lib/src/sub")).unwrap();
        std::fs::write(
            tmp.join("app/witchy.toml"),
            "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"lib\" = { path = \"../lib\" }\n",
        )
        .unwrap();
        std::fs::write(
            tmp.join("app/src/app.witchy"),
            "fn main(console: Console):\n    print(console, \"hi\")\n",
        )
        .unwrap();
        std::fs::write(
            tmp.join("lib/witchy.toml"),
            "[rune]\nname = \"lib\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            tmp.join("lib/src/lib.witchy"),
            "fn f(s: String) -> String:\n    s\n",
        )
        .unwrap();
        std::fs::write(
            tmp.join("lib/src/sub/extra.witchy"),
            "fn g() -> Int:\n    7\n",
        )
        .unwrap();

        let (linked, _stem) = crate::link_file("projects/pm/src/pm.witchy").expect("link");
        typeck::check(&linked).expect("typeck");
        let (out, code) = interpreter::run_module_exit(
            linked,
            &tmp,
            Vec::new(),
            vec!["lock".into(), "app".into()],
            None,
        )
        .expect("run");
        assert_eq!(code, 0, "lock failed: {out:?}");
        let lock = std::fs::read_to_string(tmp.join("app/witchy.lock")).unwrap();
        let store_hash = crate::pm::store::RuneSource::read_dir(&tmp.join("lib"))
            .unwrap()
            .hash();
        assert!(
            lock.contains(&store_hash),
            "nested-src lock hash must match the store hash {store_hash}: {lock:?}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The `crypto.rune_hash` native primitive (the witchy-facing content address)
    /// is byte-identical to coven's store hashing — the guarantee that makes the
    /// self-hosted `lock`/`verify` interoperate with the Rust toolchain.
    #[test]
    fn rune_hash_native_matches_the_store_byte_for_byte() {
        use crate::interpreter::Value;
        let dir = std::path::Path::new("examples/projects/ledger/money");
        let src = crate::pm::store::RuneSource::read_dir(dir).unwrap();
        let paths: Vec<Value> = src.files.iter().map(|(p, _)| Value::Str(p.clone())).collect();
        let contents: Vec<Value> = src
            .files
            .iter()
            .map(|(_, b)| Value::Str(String::from_utf8_lossy(b).into_owned()))
            .collect();
        let f = crate::native::lookup("crypto.rune_hash").expect("native rune_hash");
        let got = f(&[Value::List(paths), Value::List(contents)]).unwrap();
        assert_eq!(got, Value::Str(src.hash()));
    }

    /// coven, the registry, is self-hosted in witchy (`projects/coven`). A record
    /// it signs must verify under the *Rust* registry verifier — proving the
    /// witchy coven's canonical signing payload is byte-identical to
    /// `Record::signing_payload` and its ed25519 signature interoperates with the
    /// Rust toolchain. This record was produced by `projects/coven/src/coven.witchy`
    /// signing `acme/money@1.0.0` with the fixed seed `..01` (deterministic: the
    /// provenance carries no timestamp).
    #[test]
    fn coven_witchy_signed_record_verifies_under_the_rust_verifier() {
        // The registry root public key for the seed = 31×0x00 then 0x01.
        let rootpub = "4cb5abf6ad79fbf5abbccafcc269d85cd2651ed4b885b5869f241aedf0a5ba29";
        let record_json = r#"{"name":"acme/money","version":"1.0.0","state":"staged","hash":"sha256:000690f7a340df21196bf7dfa8447e0fb5877a4afd84c79529b292067d288fd6","runtime_footprint":[],"build_footprint":[],"determinism":"guaranteed","uploaded_by":"ci-bot","promoted_by":null,"second_factor":null,"provenance":"uploader=ci-bot|hash=sha256:000690f7a340df21196bf7dfa8447e0fb5877a4afd84c79529b292067d288fd6","released_at":0,"sig":"bc9a32c54c0acf85fda29b4fba71caca659855c840052a1de8d9c42ffcb1515806db555c741bcf456abf069aba7ca1c8cf4c297a068db94b332a5d20686c7c03"}"#;
        let record: crate::pm::registry::Record =
            serde_json::from_str(record_json).expect("deserialize witchy-coven record");
        crate::pm::registry::verify_record_with(rootpub, &record)
            .expect("a witchy-signed coven record must verify under the Rust verifier");
        // Tamper with a signed field: the signature must now fail (payload changed).
        let mut tampered = record.clone();
        tampered.runtime_footprint = vec!["Net".to_string()];
        assert!(
            crate::pm::registry::verify_record_with(rootpub, &tampered).is_err(),
            "a tampered footprint must break the signature"
        );
    }

    /// The witchy coven's TUF snapshot + timestamp roles (rollback + freeze
    /// protection) must also verify under the *Rust* TUF verifier — proving the
    /// canonical JSON the witchy server signs is byte-identical to serde's, so
    /// the whole signed-metadata chain interoperates. These were produced by
    /// `projects/coven/src/coven.witchy` publishing `acme/money@1.0.0` with the
    /// fixed seed `..01`.
    #[test]
    fn coven_witchy_tuf_metadata_verifies_under_the_rust_verifier() {
        use crate::pm::tuf::{verify_signed, Signed, Snapshot, Timestamp};
        let rootpub = "4cb5abf6ad79fbf5abbccafcc269d85cd2651ed4b885b5869f241aedf0a5ba29";
        let snap_json = r#"{"signed":{"version":1,"created":1780965897,"targets":{"acme/money@1.0.0":"sha256:427b88bebaa96a14bbd7531ad2d4b8fd992aba05cbee3ef7beedf47226da7ee6"}},"sig":"af8f2d97d3fa25d029188687f1dc1cf1b18b86c0ac2999dd342cb649429971119b2326ae6d47638f6aafb44137adabac7f3a31b724dfb7ab99f228e4e4d0270f"}"#;
        let snap: Signed<Snapshot> =
            serde_json::from_str(snap_json).expect("deserialize witchy snapshot");
        assert!(
            verify_signed(rootpub, &snap),
            "a witchy TUF snapshot must verify under the Rust verifier"
        );

        let ts_json = r#"{"signed":{"snapshot_version":1,"snapshot_hash":"sha256:851ea55f915edb8b5726a9351b1ff88e3a2f4b6a03d63825f2a6d3f2d54863df","expires":1781052297},"sig":"e2d615d5ed749f0335133bae15b420518c318e8930e522b4a855d6b0d1037c1d8a2af98fccf70104335c3358c081a7e4e8bb929c85bcc3c0aae3a8f7d0031d0d"}"#;
        let ts: Signed<Timestamp> =
            serde_json::from_str(ts_json).expect("deserialize witchy timestamp");
        assert!(
            verify_signed(rootpub, &ts),
            "a witchy TUF timestamp must verify under the Rust verifier"
        );

        // Rollback tamper: bumping the snapshot version breaks the signature.
        let mut bad = snap;
        bad.signed.version = 99;
        assert!(
            !verify_signed(rootpub, &bad),
            "a tampered snapshot version must break the signature"
        );
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
    print(console, to_string(list.length(ps)))
    let first = list.at(ps, 0)
    let (n, s) = first
    print(console, (to_string(n) <> s))
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
    print(console, to_string(list.index_of(words, "ccc")))
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
    print(console, to_string(result.unwrap_or(compute(10, 2), (0 - 1))))
    print(console, to_string(result.unwrap_or(compute(10, 0), (0 - 1))))
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
    print(console, to_string(list.fold(xs, 0, fn(a: Int, b: Int): (a + b))))
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
    print(console, "sum: ${to_string(age + 10)}")
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
        fs = list.push(fs, fn(x: Int): (x + i))
    let f0 = list.at(fs, 0)
    let f2 = list.at(fs, 2)
    print(console, to_string(f0(10)))
    print(console, to_string(f2(10)))
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
    print(console, to_string(call0(fn(): 42)))
    let base = 100
    print(console, to_string(call0(fn(): (base + 1))))
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
    print(console, to_string(apply(h, 5)))
    print(console, to_string(apply(h, 20)))
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
    print(console, to_string(sum))
    var x = 100
    x = (x - 30)
    x = (x * 2)
    x = (x / 7)
    x = (x % 5)
    print(console, to_string(x))
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
    print(console, (("[" <> string.replace("abc", "", "-")) <> "]"))
    print(console, string.replace("abc", "x", "y"))
    print(console, string.replace("aaa", "a", "bb"))
    print(console, string.replace("hello world", "o", "0"))
    var d = dict.new()
    d = dict.insert(d, 1, 100)
    d = dict.insert(d, 2, 200)
    d = dict.insert(d, 1, 111)
    print(console, to_string(dict.get_or(d, 1, 0)))
    print(console, to_string(dict.get_or(d, 2, 0)))
    print(console, to_string(dict.size(d)))
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
    print(console, to_string((0 - (7 / 2))))
    print(console, to_string(((0 - 7) % 2)))
    print(console, to_string((7 / (0 - 2))))
    print(console, to_string((7 % (0 - 2))))
    print(console, to_string(((0 - 7) / (0 - 2))))
    var d = dict.new()
    d = dict.insert(d, "k", 1)
    d = dict.insert(d, "k", 2)
    print(console, to_string(dict.get_or(d, "k", 0)))
    print(console, to_string(dict.size(d)))
    d = dict.remove(d, "missing")
    print(console, to_string(dict.size(d)))
    print(console, to_string(dict.get_or(d, "absent", 99)))
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
    print(console, to_string((list.at(fns, 0))(5)))
    print(console, to_string((list.at(fns, 1))(5)))
    let pick = true
    print(console, to_string((if pick: fn(x: Int): (x + 100) else: fn(x: Int): x)(7)))
    let b = Box(fn(x: Int): (x * 3), 7)
    print(console, to_string(((b).f)((b).n)))
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
    print(console, to_string(if is_even(10): 1 else: 0))
    print(console, to_string(if is_odd(7): 1 else: 0))
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
    print(console, to_string(((l).from).x))
    print(console, to_string(((l).to).y))
    let l2 = Line(from: Point(10, 20), ..l)
    print(console, to_string(((l2).from).x))
    print(console, to_string(((l2).to).y))
    print(console, to_string(((l).from).x))
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
    print(console, to_string(sum_tree(t)))
    print(console, to_string(depth(t)))
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
        print(console, to_string(q))
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
    print(console, to_string(list.length(triples)))
    var total = 0
    for t in triples:
        let (a, b, c) = t
        total = total + c
    print(console, to_string(total))
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
        print(console, to_string(p))
    let upper = [x * 10 + y for x in [1, 2, 3] for y in [1, 2, 3] if y > x]
    for p in upper:
        print(console, to_string(p))
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
    print(console, to_string(total))
    var kept = 0
    for y in [1, 2, 3, 4]:
        match y:
            2 ->
                continue
            _ -> 0
        kept = (kept + y)
    print(console, to_string(kept))
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
    print(console, to_string(sum))
    var i = 0
    var found = 0
    while (i < 100):
        i = (i + 1)
        if (i < 10):
            continue
        found = i
        break
    print(console, to_string(found))
    var count = 0
    for a in [1, 2, 3]:
        for b in [1, 2, 3]:
            if (b == 2):
                break
            count = (count + 1)
    print(console, to_string(count))
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
        print(console, to_string(i))
    print(console, to_string(list.length(0..=0)))
    print(console, to_string(list.length(5..=2)))
    print(console, to_string(list.length([n for n in 1..=4])))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "inclusive range diverged");
        assert_eq!(run_on_wasm(src), vec!["1", "2", "3", "4", "5", "1", "0", "4"]);
    }

    #[test]
    fn range_operator_backends_agree() {
        let src = r#"
fn main(console: Console):
    for i in 0..5:
        print(console, to_string(i))
    let squares = [x * x for x in 1..5]
    for s in squares:
        print(console, to_string(s))
    print(console, to_string(list.length(3..3)))
    print(console, to_string(list.length(2..(1 + 4))))
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
        print(console, to_string(s))
    let evens = [n for n in [1, 2, 3, 4, 5, 6] if n % 2 == 0]
    for e in evens:
        print(console, to_string(e))
    print(console, to_string(list.length([x for x in [] if x > 0])))
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
        (n, "stop") -> ("stop@" <> to_string(n))
        (n, s) -> ((s <> "=") <> to_string(n))

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
        fns = list.push(fns, fn(x: Int): (x + captured))
        i = (i + 1)
    for f in fns:
        print(console, to_string(f(10)))
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
    print(console, to_string(first_of(pi)))
    print(console, first_of(ps))
    print(console, second_of(ps))
    print(console, to_string(first_of(pm)))
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
    print(console, to_string((p2).x))
    print(console, to_string((p2).y))
    let cond = true
    let p3 = Point(y: 99, ..(if cond: (l).from else: (l).to))
    print(console, to_string((p3).x))
    print(console, to_string((p3).y))
    let l2 = Line(from: Point(x: 7, ..(l).to), ..l)
    print(console, to_string(((l2).from).x))
    print(console, to_string(((l2).from).y))
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
        print(console, to_string(((p).x + (p).y)))
    for q in [P(10, 1), P(20, 2)]:
        print(console, to_string((q).x))
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
    print(console, to_string(result))
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
    print(console, to_string(string.char_count("hello")))
    print(console, to_string(string.length("hello")))
    print(console, to_string(string.char_count("café")))
    print(console, to_string(string.length("café")))
    print(console, to_string(string.char_count("")))
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
    print(console, to_string(list.length(ws)))
    for w in ws:
        print(console, w)
    print(console, to_string(list.length(string.words("   "))))
    print(console, to_string(list.length(string.words(""))))
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
    let cs = string.chars("café")
    print(console, to_string(list.length(cs)))
    for c in cs:
        print(console, c)
    print(console, to_string(list.length(string.chars(""))))
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
    print(console, to_string(string.count("banana", "a")))
    print(console, to_string(string.count("banana", "an")))
    print(console, to_string(string.count("aaaa", "aa")))
    print(console, to_string(string.count("abc", "x")))
    print(console, to_string(string.count("abc", "")))
    print(console, to_string(string.count("aéaéa", "éa")))
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
    dict.insert(d, k, v)

fn lookup(d: Dict(String, Int), k: String) -> Int:
    dict.get_or(d, k, (0 - 1))

fn main(console: Console):
    var d = dict.new()
    d = put(d, "apple", 1)
    d = put(d, "banana", 2)
    print(console, to_string(lookup(d, ("ap" <> "ple"))))
    print(console, to_string(lookup(d, "banana")))
    print(console, to_string(lookup(d, "cherry")))
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
    print(console, to_string(size))
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
    print(console, to_string(list.sum(list.range(5))))
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
    print(console, to_string(result.unwrap_or(option.ok_or(Some(5), "none"), 0)))
    print(console, to_string(result.is_err(option.ok_or(None, "none"))))
    print(console, to_string(result.unwrap_or(option.ok_or_else(Some(9), fn(): "none"), 0)))
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
    print(console, to_string(result.unwrap_or(result.flatten(nested(5)), 0)))
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
    print(console, to_string(option.unwrap_or(result.ok(check(5)), 0)))
    print(console, to_string(option.is_none(result.ok(check((0 - 1))))))
    print(console, to_string(option.is_none(result.err(check(5)))))
    print(console, to_string(string.length(option.unwrap_or(result.err(check((0 - 1))), ""))))
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
        print(console, to_string(x))
    let u = list.unique([1, 2, 2, 3, 1, 4, 3])
    print(console, to_string(list.length(u)))
    for x in u:
        print(console, to_string(x))
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
        Some(a) -> print(console, to_string((a).balance))
        None -> print(console, "none")
    let xs = mk()
    for p in xs:
        print(console, to_string((p).x))
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
        Ok(v) -> print(console, to_string(v))
        Err(e) -> print(console, e)
    match process((0 - 1)):
        Ok(v) -> print(console, to_string(v))
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
        JNum(n) -> to_string(n)
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
    print(console, to_string(total))
    match list.max_by(cart, fn(a: Item, b: Item): (line_total(a) < line_total(b))):
        Some(it) -> print(console, (it).name)
        None -> print(console, "none")
    let multi = list.filter(cart, fn(it: Item): ((it).qty > 1))
    for it in multi:
        print(console, (it).name)
    match list.find(cart, fn(it: Item): ((it).name == "bread")):
        Some(it) -> print(console, to_string((it).price))
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
        print(console, to_string((p).x))
    for p in list.map(raws, fn(r: Raw): Point((r).b, (r).a)):
        print(console, to_string((p).y))
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
        print(console, to_string((p).y))
    for p in list.reverse(ps):
        print(console, to_string((p).x))
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
        Some(acc) -> print(console, to_string((acc).balance))
        None -> print(console, "none")
    match list.head(accounts):
        Some(acc) -> print(console, to_string((acc).id))
        None -> print(console, "none")
    match list.find(accounts, fn(a: Account): ((a).balance > 999)):
        Some(acc) -> print(console, to_string((acc).id))
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
        Some(a) -> print(console, to_string((a).balance))
        None -> print(console, "none")
    match lookup((0 - 1)):
        Some(a) -> print(console, to_string((a).balance))
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
    print(console, to_string(f(Circle(Point(3, 4)))))
    print(console, to_string(f(Circle(Point(10, 1)))))
    print(console, to_string(f(Origin)))
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
    print(console, to_string(describe(Circle(Point(3, 4)))))
    print(console, to_string(describe(Rect(5, 6))))
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
    print(console, to_string(option.unwrap_or(mx, 0)))
    print(console, to_string(option.is_none(list.reduce([], fn(a: Int, b: Int): (a + b)))))
    let sum = list.reduce([10, 20, 30], fn(a: Int, b: Int): (a + b))
    print(console, to_string(option.unwrap_or(sum, 0)))
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
    print(console, to_string(option.unwrap_or(r, (0 - 1))))
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
    print(console, to_string(option.unwrap_or(list.max_by(xs, fn(a: Int, b: Int): (a < b)), 0)))
    print(console, to_string(option.unwrap_or(list.min_by(xs, fn(a: Int, b: Int): (a < b)), 0)))
    print(console, to_string(option.unwrap_or(list.max_by(xs, fn(a: Int, b: Int): ((0 - a) < (0 - b))), 0)))
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
    print(console, to_string(option.unwrap_or(list.min([3, 1, 4, 1, 5]), 0)))
    print(console, to_string(option.unwrap_or(list.max([3, 1, 4, 1, 5]), 0)))
    print(console, to_string(option.is_none(list.min([]))))
    print(console, to_string(option.unwrap_or(list.position([10, 20, 30], 20), (0 - 1))))
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
    print(console, to_string(option.unwrap_or(list.head([10, 20]), 0)))
    print(console, to_string(option.unwrap_or(list.head([]), (0 - 1))))
    print(console, to_string(option.unwrap_or(list.last([10, 20]), 0)))
    print(console, to_string(option.unwrap_or(list.get([10, 20, 30], 1), 0)))
    print(console, to_string(option.unwrap_or(list.get([10], 5), (0 - 1))))
    print(console, to_string(option.unwrap_or(list.find([1, 3, 4], fn(n: Int): ((n % 2) == 0)), (0 - 1))))
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
    print(console, to_string(list.head_or([10, 20, 30], 0)))
    print(console, to_string(list.head_or([], (0 - 1))))
    print(console, to_string(list.last_or([10, 20, 30], 0)))
    print(console, to_string(list.last_or([], (0 - 1))))
    print(console, to_string(list.find_or([1, 3, 4, 7], fn(n: Int): ((n % 2) == 0), (0 - 1))))
    print(console, to_string(list.find_or([1, 3, 5], fn(n: Int): ((n % 2) == 0), (0 - 1))))
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
    print(console, to_string(list.length(ws)))
    for w in ws:
        print(console, to_string(list.sum(w)))
    print(console, to_string(list.length(list.windows([1, 2], 5))))
    print(console, to_string(list.length(list.windows([1, 2, 3], 0))))
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
    print(console, to_string(list.sum(a)))
    print(console, to_string(list.sum(b)))
    let (c, d) = list.split_at([1, 2], 5)
    print(console, to_string(list.sum(c)))
    print(console, to_string(list.length(d)))
    let (e, f) = list.split_at([1, 2, 3], 0)
    print(console, to_string(list.length(e)))
    print(console, to_string(list.sum(f)))
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
    print(console, to_string(list.length(cs)))
    for c in cs:
        print(console, to_string(list.sum(c)))
    print(console, to_string(list.sum(list.tail([1, 2, 3]))))
    print(console, to_string(list.sum(list.drop_last([1, 2, 3]))))
    print(console, to_string(list.length(list.tail([]))))
    print(console, to_string(list.length(list.drop_last([]))))
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
    print(console, to_string(list.sum_by(cart, fn(it: Item): ((it).price * (it).qty))))
    print(console, to_string(list.sum_by([1, 2, 3, 4], fn(n: Int): (n * n))))
    print(console, to_string(list.sum_by([], fn(n: Int): n)))
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
    print(console, to_string(list.product([1, 2, 3, 4])))
    print(console, to_string(list.product([])))
    let s = list.slice([10, 20, 30, 40, 50], 1, 4)
    for x in s:
        print(console, to_string(x))
    let running = list.scan([1, 2, 3], 0, fn(acc: Int, n: Int): (acc + n))
    for x in running:
        print(console, to_string(x))
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
    print(console, to_string(h(10)))
    print(console, to_string((func.flip(sub))(3, 10)))
    print(console, to_string((func.constant(42))(999)))
    print(console, to_string(func.identity(7)))
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
    print(console, to_string(h(10)))
    print(console, to_string((compose(inc, double))(10)))
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
    print(console, to_string(f(5)))
    let g = inc
    print(console, to_string(g(g(g(0)))))
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
        print(console, to_string(y))
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
    print(console, to_string((make_adder(10))(5)))
    print(console, to_string(((make_mul(2))(3))(4)))
    print(console, to_string((fn(n: Int): (n * n))(7)))
    print(console, to_string(twice(make_adder(1), 10)))
    print(console, to_string((make_adder(10))((make_adder(2))(3))))
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
    print(console, to_string(total))
    let make_adder = fn(x: Int): fn(y: Int): (x + y)
    let add3 = make_adder(3)
    print(console, to_string(add3(4)))
    print(console, to_string((make_adder(100))(1)))
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
    print(console, to_string(list.length(string.split("abc", ""))))
    print(console, to_string(list.length(string.split("abc", "x"))))
    print(console, to_string(list.length(string.split("a,b,c", ","))))
    print(console, (("[" <> string.substring("", 0, 5)) <> "]"))
    print(console, (("[" <> string.substring("hello", 3, 1)) <> "]"))
    print(console, string.substring("hello", 2, 100))
    print(console, to_string(string.index_of("hello", "")))
    print(console, to_string(string.index_of("hello", "z")))
    print(console, (("[" <> (("" <> "x") <> "")) <> "]"))
    print(console, to_string(string.length("")))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "string edge cases diverged");
    }

    #[test]
    fn trim_backends_agree() {
        // trim now compiles: leading/trailing ASCII whitespace (spaces, tabs,
        // newlines, CRs) is stripped; an all-whitespace string trims to "".
        let src = r#"
fn main(console: Console):
    print(console, string.trim("  hello  "))
    print(console, string.trim("\t\nfoo\r\n"))
    print(console, string.trim("nospaces"))
    print(console, string.trim("   "))
    print(console, to_string(string.length(string.trim("  a b  "))))
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
            print(console, to_string(int_at(j, "user.age")))
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
    print(console, to_string(list.sum(xs)))
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
        Ok(u) -> url.scheme(u) <> " " <> url.host(u) <> " " <> to_string(url.port(u)) <> " " <> url.path(u)
        Err(e) -> "invalid: " <> e
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
                "invalid: missing `scheme://` in: notaurl"
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
        to_string(self)

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
    print(console, to_string(area(Circle(2))))
    print(console, to_string(area(Square(3))))
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
        to_string(self)

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
    print(console, to_string(compare(3, 5)))
    print(console, to_string(less(3, 5)))
    print(console, to_string(greater_equal(5, 5)))
    print(console, to_string(compare(1.5, 0.5)))
    print(console, to_string(less(1.5, 2.5)))
    print(console, to_string(compare(Money(10), Money(4))))
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
            Point(x, y) -> (((("(" <> to_string(x)) <> ", ") <> to_string(y)) <> ")")

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
    print(console, to_string(pick_max(3, 7)))
    print(console, to_string(pick_max(20, 5)))
    print(console, to_string(unbox(pick_max(Box(4), Box(11)))))
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
    print(console, to_string(ord.max_of((-5), 3)))
    print(console, to_string(ord.min_of(8, 2)))
    print(console, to_string(ord.clamp(10, 0, 5)))
    print(console, to_string(ord.clamp(0, 3, 9)))
    print(console, to_string(unbox(ord.max_of(Box(4), Box(11)))))
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
    print(console, to_string(ord.maximum([3, 7, 2, 9, 4], 0)))
    print(console, to_string(ord.minimum([3, 7, 2, 9, 4], 100)))
    print(console, to_string(ord.maximum([], 42)))
    print(console, to_string(unbox(ord.maximum([Box(2), Box(8), Box(5)], Box(0)))))
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
    print(console, to_string(total))
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
        to_string(self)

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
    print(console, to_string(x))
    print(console, to_string(y))
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
    print(console, to_string(http.status(r)))
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
    print(console, to_string(http.status(r)))
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
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        });
        let program = format!(
            r#"
import http
fn main(console: Console, net: Net):
    let r = http.post(net, "127.0.0.1", {port}, "/echo", "hello body")
    print(console, to_string(http.status(r)))
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
            print(console, to_string(option.unwrap_or(json.as_int(field(j, "version")), 0)))
            print(console, to_string(elem_int(j, "items", 1)))
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
    print(console, to_string(255))
    print(console, to_string(10))
    print(console, to_string((255 & 15)))
    print(console, to_string((12 | 3)))
    print(console, to_string(65535))
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
    print(console, to_string(string.to_int("42")))
    print(console, to_string(string.to_int("-17")))
    print(console, to_string(string.to_int("  123  ")))
    print(console, to_string(string.to_int("+8")))
    print(console, to_string(string.to_int("0")))
    print(console, to_string((string.to_int("1000000") + 1)))
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["42", "-17", "123", "8", "0", "1000001"]);
    }

    #[test]
    fn bitwise_not_backends_agree() {
        // ~x = -x-1 (width-independent), so it agrees across backends.
        let src = r#"
fn main(console: Console):
    print(console, to_string((~0)))
    print(console, to_string((~5)))
    print(console, to_string((~(0 - 1))))
    print(console, to_string((255 & (~15))))
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
    print(console, to_string((12 & 10)))
    print(console, to_string((12 | 10)))
    print(console, to_string((12 ^ 10)))
    print(console, to_string((1 << 4)))
    print(console, to_string((256 >> 2)))
    print(console, to_string(((5 & 3) | 8)))
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
    print(console, to_string(side(Circle(5))))
    print(console, to_string(side(Square(7))))
    print(console, to_string(side(Rect(3, 4))))
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
        print(console, to_string(x))
    print(console, to_string(x))
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
    print(console, to_string(list.length((b).items)))
    var total = 0
    for x in (b).items:
        total = (total + x)
    print(console, to_string(total))
    print(console, to_string(list.at((b).items, 1)))
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
    print(console, to_string(((o).inner).v))
    let o2 = Outer(inner: Inner((((o).inner).v + 1)), ..o)
    print(console, to_string(((o2).inner).v))
    print(console, (o).name)
    print(console, to_string(((o).inner).v))
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
    print(console, to_string(x))
    print(console, to_string(y))
    var acc = 0
    var i = 1
    while (i < 5):
        bump_by(acc, i)
        i = (i + 1)
    print(console, to_string(acc))
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

    /// The dispatch example: the full actor message model in one program —
    /// Float and String fields, a List(String) summary, Float state averaging
    /// across messages, and a Subject delivered IN a message (delegation: the
    /// Router is introduced to the Reporter at runtime). The relayed Alert is
    /// enqueued mid-drain, so it lands after the directly-queued Summary.
    #[test]
    fn dispatch_example_delegates_and_aggregates() {
        assert_eq!(
            interp(include_str!("../examples/dispatch.witchy")),
            vec![
                "mean 2",
                "mean 3",
                "routing thermostat",
                "summary: 2 samples",
                "summary: 1 alert",
                "[alert 1] thermostat at 99.5",
            ]
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
            vec!["3 remainder 2", "7 spells seven", "just the remainder: 2", "2 3"]
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

