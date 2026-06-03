//! Witchy runtime spike — proving the core thesis: an actor in an isolated
//! WASM VM can do nothing beyond the capabilities it was explicitly granted.
//!
//! The "actors" here are hand-written WebAssembly standing in for compiled
//! witchy code; the point of the spike is the security substrate, not the
//! language surface yet.

mod actor_system;
mod ast;
mod codegen;
mod interpreter;
mod lexer;
mod linker;
mod parser;
mod runtime;
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

fn main() -> wasmtime::Result<()> {
    // `witchy <file.witchy>` runs a program; with no argument, run the demos.
    if let Some(path) = std::env::args().nth(1) {
        match execute_file(&path) {
            Ok(output) => {
                for line in output {
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

    let mut rt = Runtime::new()?;

    println!("== M2: capability gating ==");

    // Granted the `print` capability — works.
    let mut greeter = rt.spawn(GREETER, Capabilities { print: true, send: false, print_int: false }, 4)?;
    greeter.run()?;

    // Granted nothing — must fail to instantiate because `witchy.print` is not
    // linked into its VM.
    match rt.spawn(MALICIOUS, Capabilities::none(), 4) {
        Ok(_) => println!("!! SECURITY FAILURE: ungranted actor was allowed to instantiate"),
        Err(e) => println!("DENIED (as designed): {e}"),
    }

    println!("\n== M3: message passing across isolated VMs ==");

    // Logger can print + recv, but cannot send.
    let mut logger = rt.spawn(LOGGER, Capabilities { print: true, send: false, print_int: false }, 4)?;
    // Sender can only send.
    let mut sender = rt.spawn(
        sender_src(logger.id),
        Capabilities { print: false, send: true, print_int: false },
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
    run_witchy("witchy expression evaluator (recursive ADT)", include_str!("../examples/eval.witchy"));
    run_witchy("witchy bank (records + lists + Result)", include_str!("../examples/bank.witchy"));
    run_witchy("witchy higher-order functions (closures)", include_str!("../examples/higher_order.witchy"));
    run_witchy("witchy list combinators (map/filter via push)", include_str!("../examples/list_ops.witchy"));
    run_witchy("witchy dictionaries (word count)", include_str!("../examples/wordcount.witchy"));
    run_witchy("witchy dict iteration (values/pairs)", include_str!("../examples/inventory.witchy"));
    run_witchy("witchy early return (guard clauses)", include_str!("../examples/guard.witchy"));
    run_witchy("witchy string slicing (substring/index_of)", include_str!("../examples/parse_kv.witchy"));
    run_witchy("witchy actors", include_str!("../examples/actors.witchy"));
    run_witchy("witchy filesystem capability", include_str!("../examples/files.witchy"));
    run_compiled(&mut rt, "witchy compiled to WASM (ints)", include_str!("../examples/compute.witchy"));
    run_compiled(&mut rt, "witchy compiled to WASM (ADTs)", include_str!("../examples/shapes.witchy"));
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
    run_program_demo(
        "witchy list search/slice (contains/index_of/take/drop)",
        &[
            ("list", include_str!("../std/list.witchy")),
            ("list_more", include_str!("../examples/list_more.witchy")),
        ],
        "list_more",
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
                fn checked_div(a: Int, b: Int) -> Result(Int, String) {
                  match b { 0 -> Err("divide by zero")  _ -> Ok(a / b) }
                }
                fn compute(x: Int, y: Int) -> Result(Int, String) {
                  let q = checked_div(x, y)?
                  Ok(q + 1)
                }
                fn main(console: Console) {
                  print(console, int_to_string(result.unwrap_or(compute(10, 2), 0 - 1)))
                  print(console, int_to_string(result.unwrap_or(compute(10, 0), 0 - 1)))
                }
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
/// Source for a bundled standard-library module, shipped with the compiler so
/// `import list` works without a local file. A local file of the same name
/// takes precedence (see `execute_file`).
fn bundled_module(name: &str) -> Option<&'static str> {
    match name {
        "list" => Some(include_str!("../std/list.witchy")),
        "string" => Some(include_str!("../std/string.witchy")),
        "math" => Some(include_str!("../std/math.witchy")),
        "result" => Some(include_str!("../std/result.witchy")),
        "option" => Some(include_str!("../std/option.witchy")),
        _ => None,
    }
}

fn execute_file(path: &str) -> Result<Vec<String>, String> {
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
                None => return Err(format!("cannot read `{}`: {e}", p.display())),
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
        return Ok(vec![msg]);
    }

    // The root `Dir` capability is anchored at the current directory (the same
    // root the demos use), independent of where the source file lives.
    interpreter::run_module(linked, Path::new("."), Vec::new()).map_err(|e| e.to_string())
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
        fn main(console: Console, net: Net) {
          let s = connect(net, "10.255.255.1:80")
          send_line(s, "x")
        }
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

    #[test]
    fn compute_runs_on_wasm() {
        assert_eq!(
            run_on_wasm(include_str!("../examples/compute.witchy")),
            vec!["217"]
        );
    }

    #[test]
    fn shapes_runs_on_wasm() {
        assert_eq!(
            run_on_wasm(include_str!("../examples/shapes.witchy")),
            vec!["325"]
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

    /// A library imported into a program brings its functions into scope but no
    /// authority: `lib` has no capability parameters, so it can only compute.
    #[test]
    fn imported_library_is_pure_and_confined() {
        let lib = r#"
            fn label(n: Int) -> String {
              if n < 0 { "neg" } else { "nonneg" }
            }
        "#;
        let main = r#"
            import lib
            fn main(console: Console) {
              print(console, lib.label(-2))
              print(console, lib.label(7))
            }
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

    /// Every example must at least compile (parse + link + type-check) and run
    /// to completion through the CLI without an error — whether it prints, just
    /// returns a value, or is a library/actor file with no `main`.
    #[test]
    fn all_examples_run_via_cli() {
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir("examples")
            .expect("examples directory")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("witchy"))
            .collect();
        files.sort();
        assert!(!files.is_empty(), "no examples found");
        for path in files {
            let p = path.to_str().unwrap();
            let result = crate::execute_file(p);
            assert!(result.is_ok(), "example `{p}` failed: {result:?}");
        }
    }

    #[test]
    fn compute_example_returns_value() {
        assert_eq!(
            crate::execute_file("examples/compute.witchy").unwrap(),
            vec!["217"]
        );
    }

    #[test]
    fn shapes_example_returns_value() {
        assert_eq!(
            crate::execute_file("examples/shapes.witchy").unwrap(),
            vec!["325"]
        );
    }

    #[test]
    fn files_example_reads_sandboxed_file() {
        assert_eq!(
            crate::execute_file("examples/files.witchy").unwrap(),
            vec!["hello from a sandboxed Dir capability"]
        );
    }

    /// `import list` resolves to the bundled standard library (no local file),
    /// links, type-checks, and runs end to end through the CLI.
    #[test]
    fn std_library_resolves_and_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/std_demo.witchy").unwrap(),
            vec!["30", "3"]
        );
    }

    /// Sorting with a comparator closure, end to end through the bundled std.
    #[test]
    fn sort_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/sort.witchy").unwrap(),
            vec!["1,1,3,4,5", "5,4,3,1,1"]
        );
    }

    /// The bundled `math` module resolves and computes via the CLI.
    #[test]
    fn math_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/math_demo.witchy").unwrap(),
            vec!["7", "5", "10", "1024", "12"]
        );
    }

    /// Float math: the `sqrt` builtin and the `math` module's Float helpers.
    #[test]
    fn floats_run_via_cli() {
        assert_eq!(
            crate::execute_file("examples/floats.witchy").unwrap(),
            vec!["4", "3.5", "5", "1"]
        );
    }

    /// The list module's search/slice helpers via the CLI.
    #[test]
    fn list_more_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/list_more.witchy").unwrap(),
            vec!["true", "3", "-1", "20", "30"]
        );
    }

    /// `contains`/`index_of` are generic — they work on Strings too (by value).
    #[test]
    fn list_contains_is_generic_over_element_type() {
        let client = r#"
            import list
            fn main(console: Console) {
              let words = ["a", "bb", "ccc"]
              print(console, to_string(list.contains(words, "bb")))
              print(console, int_to_string(list.index_of(words, "ccc")))
            }
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
            crate::execute_file("examples/option_std.witchy").unwrap(),
            vec!["10", "-1"]
        );
    }

    /// The bundled `result` module supplies the type `?` recognizes, plus
    /// helpers, when linked against a client.
    #[test]
    fn result_module_links_with_try_and_helpers() {
        let client = r#"
            import result
            fn checked_div(a: Int, b: Int) -> Result(Int, String) {
              match b { 0 -> Err("zero")  _ -> Ok(a / b) }
            }
            fn compute(x: Int, y: Int) -> Result(Int, String) {
              let q = checked_div(x, y)?
              Ok(q + 1)
            }
            fn main(console: Console) {
              print(console, int_to_string(result.unwrap_or(compute(10, 2), 0 - 1)))
              print(console, int_to_string(result.unwrap_or(compute(10, 0), 0 - 1)))
              print(console, to_string(result.is_ok(compute(10, 0))))
            }
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
            crate::execute_file("examples/text.witchy").unwrap(),
            vec!["ALICE | BOB | CAROL", "===", "alice,***,carol"]
        );
    }

    /// The bundled `list` module type-checks and links against a client program.
    #[test]
    fn bundled_list_module_links() {
        let client = r#"
            import list
            fn main(console: Console) {
              let xs = list.map(list.range(4), fn(n: Int) { n + 1 })
              print(console, int_to_string(list.fold(xs, 0, fn(a: Int, b: Int) { a + b })))
            }
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
            r#"fn shout(s: String) -> String { "HI " <> s }"#,
        )
        .unwrap();
        let app = dir.join("app.witchy");
        std::fs::write(
            &app,
            "import strutil\nfn main(console: Console) { print(console, strutil.shout(\"x\")) }",
        )
        .unwrap();

        let out = crate::execute_file(app.to_str().unwrap()).unwrap();
        assert_eq!(out, vec!["HI x"]);
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
