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
    // `witchy --bench` compares interpreter vs compiled execution.
    if std::env::args().nth(1).as_deref() == Some("--bench") {
        return run_benchmarks();
    }
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
        fn fib(n: Int) -> Int {
          if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
        }
        fn main() -> Int { fib(30) }
    "#;
    // The accumulator is kept under 10^6 (`% 1000000`) so it stays well within
    // the compiled backend's 32-bit `Int`; otherwise this sum would overflow in
    // the compiled run (which wraps at 2^31) while the i64 interpreter would not,
    // making the two columns incomparable. (Compiled `Int` is i32, a deliberate
    // divergence from the interpreter's i64 — see `ty_kind` in codegen.)
    let loop_sum = r#"
        fn main() -> Int {
          var sum = 0
          var i = 0
          while i < 1000000 {
            sum = (sum + i) % 1000000
            i = i + 1
          }
          sum
        }
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
                fn main(console: Console) {
                  var acc = 0
                  var i = 0
                  while i < 12 {
                    if i % 2 == 0 { acc = acc + i } else { acc = acc - i }
                    i = i + 1
                  }
                  print(console, int_to_string(acc))
                }
                "#,
            ),
            (
                "records + update + field access",
                r#"
                type Point { x: Int, y: Int }
                fn main(console: Console) {
                  let p = Point(3, 4)
                  let q = update p { x: p.x + 10 }
                  print(console, int_to_string(q.x * q.y))
                }
                "#,
            ),
            (
                "lists + recursion + head/tail match",
                r#"
                fn sum(xs: List(Int)) -> Int {
                  match xs { [] -> 0  [h, ..t] -> h + sum(t) }
                }
                fn main(console: Console) {
                  print(console, int_to_string(sum([1, 2, 3, 4, 5])))
                }
                "#,
            ),
            (
                "ADTs + match",
                r#"
                type Shape { Circle(Int)  Rect(Int, Int) }
                fn area(s: Shape) -> Int {
                  match s { Circle(r) -> 3 * r * r  Rect(w, h) -> w * h }
                }
                fn main(console: Console) {
                  print(console, int_to_string(area(Circle(5)) + area(Rect(3, 4))))
                }
                "#,
            ),
            (
                "capturing closures + higher-order",
                r#"
                fn apply(f: fn(Int) -> Int, x: Int) -> Int { f(x) }
                fn main(console: Console) {
                  let k = 100
                  print(console, int_to_string(apply(fn(n: Int) { n + k }, 5)))
                }
                "#,
            ),
            (
                "dicts",
                r#"
                fn main(console: Console) {
                  var d = dict_new()
                  d = insert(d, "a", 1)
                  d = insert(d, "b", 2)
                  d = insert(d, "a", 9)
                  print(console, int_to_string(get_or(d, "a", 0) + size(d)))
                }
                "#,
            ),
            (
                "strings",
                r#"
                fn main(console: Console) {
                  print(console, replace("a,b,c", ",", "-"))
                  print(console, int_to_string(index_of("hello", "l")))
                  print(console, substring("hello", 1, 4))
                  for w in split("the cat sat", " ") { print(console, w) }
                }
                "#,
            ),
            (
                "string equality across a List(String) parameter",
                r#"
                fn count_matches(words: List(String), target: String) -> Int {
                  var n = 0
                  for w in words {
                    if w == target { n = n + 1 }
                  }
                  n
                }
                fn main(console: Console) {
                  let words = split("apple banana apple cherry apple", " ")
                  print(console, int_to_string(count_matches(words, "apple")))
                }
                "#,
            ),
            (
                "string equality + ordering",
                r#"
                fn main(console: Console) {
                  let a = substring("xapple", 1, 6)
                  print(console, to_string(a == "apple"))
                  print(console, to_string(a == "apricot"))
                  print(console, to_string(a != "apricot"))
                  print(console, to_string("apple" < "banana"))
                  print(console, to_string("banana" < "apple"))
                  print(console, to_string("app" < "apple"))
                  print(console, to_string("apple" <= "apple"))
                }
                "#,
            ),
            (
                "tuples + polymorphic to_string",
                r#"
                fn main(console: Console) {
                  let (a, b) = (7, 8)
                  print(console, to_string(a + b))
                  print(console, to_string(a < b))
                  print(console, to_string("done"))
                }
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
            fn main(console: Console) {
              let xs = [5, 3, 8, 1, 9, 2]
              let rev = list.reverse(xs)
              print(console, int_to_string(at(rev, 0)) <> "," <> int_to_string(at(rev, 5)))
              print(console, int_to_string(length(list.take(xs, 3))) <> ":" <> int_to_string(at(list.take(xs, 3), 2)))
              print(console, int_to_string(at(list.drop(xs, 4), 0)))
              let sorted = list.sort_by(xs, fn(a: Int, b: Int) { a < b })
              print(console, int_to_string(at(sorted, 0)) <> ".." <> int_to_string(at(sorted, 5)))
              let pairs = list.zip([1, 2, 3], [10, 20, 30])
              let (pa, pb) = at(pairs, 1)
              print(console, int_to_string(pa + pb))
              let en = list.enumerate([100, 200])
              let (ei, ev) = at(en, 1)
              print(console, int_to_string(ei * 1000 + ev))
              let doubled = list.map(xs, fn(n: Int) { n * 2 })
              let evens = list.filter(xs, fn(n: Int) { n % 2 == 0 })
              print(console, int_to_string(list.fold(doubled, 0, fn(a: Int, b: Int) { a + b })))
              print(console, int_to_string(length(evens)))
              print(console, int_to_string(list.index_of(xs, 8)))
              print(console, to_string(list.contains(xs, 9)))
              print(console, to_string(list.any(xs, fn(n: Int) { n > 8 })))
              print(console, to_string(list.all(xs, fn(n: Int) { n > 0 })))
              print(console, int_to_string(list.sum(xs)))
              print(console, to_string(list.is_empty(xs)))
              print(console, to_string(list.is_empty(list.filter(xs, fn(n: Int) { n > 100 }))))
              print(console, int_to_string(list.count(xs, fn(n: Int) { n % 2 == 0 })))
            }
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
            type Box { Wrap(a) }
            fn unwrap(b: Box(a), default: a) -> a {
              match b { Wrap(v) -> v }
            }
            fn main(console: Console) {
              print(console, int_to_string(unwrap(Wrap(42), 0)))
              print(console, unwrap(Wrap("hello"), "none"))
            }
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
            fn parse_pos(n: Int) -> Result(Int, String) {
              if n > 0 { Ok(n) } else { Err("bad") }
            }
            fn add_two(a: Int, b: Int) -> Result(Int, String) {
              let x = parse_pos(a)?
              let y = parse_pos(b)?
              Ok(x + y)
            }
            fn main(console: Console) {
              print(console, int_to_string(result.unwrap_or(add_two(3, 4), 0)))
              print(console, int_to_string(result.unwrap_or(add_two(3, 0 - 1), 0)))
              print(console, to_string(result.is_err(add_two(0 - 5, 2))))
              print(console, to_string(result.is_ok(add_two(10, 20))))
            }
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
            fn first_even(a: Int, b: Int) -> Option(Int) {
              let x = pick_even(a)?
              let y = pick_even(b)?
              Some(x + y)
            }
            fn pick_even(n: Int) -> Option(Int) {
              if n % 2 == 0 { Some(n) } else { None }
            }
            fn main(console: Console) {
              print(console, int_to_string(option.unwrap_or(first_even(4, 6), 0)))
              print(console, int_to_string(option.unwrap_or(first_even(4, 7), 0)))
              print(console, to_string(option.is_none(first_even(3, 8))))
            }
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
            fn main(console: Console) {
              let words = ["cherry", "apple", "banana", "date", "apple"]
              let sorted = list.sort_by(words, fn(a: String, b: String) { a < b })
              for w in sorted { print(console, w) }
            }
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
            fn main(console: Console) {
              let xs = [3, 8, 1, 9, 4]
              print(console, int_to_string(list.find_index(xs, fn(n: Int) { n > 5 })))
              print(console, int_to_string(list.find_index(xs, fn(n: Int) { n > 100 })))
              print(console, int_to_string(list.find_index(xs, fn(n: Int) { n == 1 })))
            }
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
            fn main(console: Console) {
              let sums = list.zip_with([1, 2, 3], [10, 20], fn(a: Int, b: Int) { a + b })
              print(console, int_to_string(length(sums)))    // 2 (shorter)
              print(console, int_to_string(list.sum(sums)))   // 11 + 22 = 33
              let spaced = list.intersperse([5, 6, 7], 0)
              print(console, int_to_string(length(spaced)))   // 5
              print(console, int_to_string(list.sum(spaced))) // 5+0+6+0+7 = 18
              print(console, int_to_string(length(list.intersperse([9], 0))))  // 1
              print(console, int_to_string(length(list.intersperse([], 0))))   // 0
            }
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
            fn main(console: Console) {
              let xs = [1, 2, 3, 10, 4, 5]
              print(console, int_to_string(list.sum(list.take_while(xs, fn(n: Int) { n < 5 }))))
              print(console, int_to_string(list.sum(list.drop_while(xs, fn(n: Int) { n < 5 }))))
              let threes = list.repeat(7, 3)
              print(console, int_to_string(list.sum(threes)))
              print(console, int_to_string(length(threes)))
              print(console, int_to_string(length(list.repeat(9, 0))))
            }
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
            fn main(console: Console) {
              let nested = [[1, 2], [3], [4, 5, 6]]
              let flat = list.flatten(nested)
              print(console, int_to_string(length(flat)))
              print(console, int_to_string(list.sum(flat)))
              let fm = list.flat_map([1, 2, 3], fn(n: Int) { [n, n * 10] })
              print(console, int_to_string(length(fm)))
              print(console, int_to_string(list.sum(fm)))
            }
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
            fn main(console: Console) {
              print(console, int_to_string(option.unwrap_or_else(Some(5), fn() { 0 })))
              let fallback = 99
              print(console, int_to_string(option.unwrap_or_else(option.filter(Some(3), fn(n: Int) { n > 10 }), fn() { fallback })))
            }
        "#;
        let osrc = [("option", crate::bundled_module("option").unwrap()), ("main", opt)];
        assert_eq!(
            interpreter::run_program(&osrc, "main").expect("interp"),
            run_linked_on_wasm(&osrc, "main")
        );
        assert_eq!(run_linked_on_wasm(&osrc, "main"), vec!["5", "99"]);

        let res = r#"
            import result
            fn checked(n: Int) -> Result(Int, String) {
              if n > 0 { Ok(n) } else { Err("bad") }
            }
            fn main(console: Console) {
              print(console, int_to_string(result.unwrap_or_else(checked(7), fn() { 0 })))
              print(console, int_to_string(result.unwrap_or_else(checked(0 - 1), fn() { 42 })))
            }
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
            fn main(console: Console) {
              let s = Some(5)
              print(console, to_string(option.is_none(s)))
              print(console, to_string(option.is_none(option.filter(s, fn(n: Int) { n > 10 }))))
              let chained = option.and_then(s, fn(n: Int) { Some(n * 2) })
              print(console, int_to_string(option.unwrap_or(chained, 0)))
              let kept = option.filter(s, fn(n: Int) { n > 0 })
              print(console, int_to_string(option.unwrap_or(kept, 0)))
            }
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
            fn nested(n: Int) -> Option(Option(Int)) {
              if n > 0 { Some(Some(n)) } else { Some(None) }
            }
            fn main(console: Console) {
              print(console, int_to_string(option.unwrap_or(option.flatten(nested(7)), 0 - 1)))
              print(console, int_to_string(option.unwrap_or(option.flatten(nested(0)), 0 - 1)))
              match option.zip(Some(3), Some(4)) {
                Some(pair) -> {
                  let (x, y) = pair
                  print(console, int_to_string(x + y))
                }
                None -> print(console, "none")
              }
              print(console, to_string(option.is_none(option.zip(Some(1), None))))
            }
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
            fn main(console: Console) {
              print(console, int_to_string(option.unwrap_or(option.or(Some(5), Some(9)), 0)))
              print(console, int_to_string(option.unwrap_or(option.or(None, Some(9)), 0)))
              print(console, int_to_string(option.unwrap_or(option.or_else(None, fn() { Some(7) }), 0)))
              print(console, int_to_string(option.unwrap_or(option.or_else(Some(3), fn() { Some(7) }), 0)))
              print(console, int_to_string(option.map_or(Some(10), 0, fn(x: Int) { x * 2 })))
              print(console, int_to_string(option.map_or(None, 99, fn(x: Int) { x * 2 })))
            }
        "#;
        let sources = [("option", crate::bundled_module("option").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "option or/map_or diverged");
        assert_eq!(compiled, vec!["5", "9", "7", "3", "20", "99"]);
    }

    #[test]
    fn std_result_or_mapor_backends_agree() {
        // The fallback combinators mirror Option's: `or` / `or_else` keep an Ok
        // or supply an alternative (eagerly / error-aware lazily), and `map_or`
        // transforms an Ok or returns the default for an Err. Both backends agree.
        let client = r#"
            import result
            fn checked(n: Int) -> Result(Int, String) {
              if n > 0 { Ok(n) } else { Err("bad") }
            }
            fn main(console: Console) {
              print(console, int_to_string(result.unwrap_or(result.or(checked(5), Ok(9)), 0)))
              print(console, int_to_string(result.unwrap_or(result.or(checked(0 - 1), Ok(9)), 0)))
              print(console, int_to_string(result.unwrap_or(result.or_else(checked(0 - 1), fn(e: String) { Ok(string_length(e)) }), 0)))
              print(console, int_to_string(result.map_or(checked(5), 0, fn(x: Int) { x * 2 })))
              print(console, int_to_string(result.map_or(checked(0 - 1), 99, fn(x: Int) { x * 2 })))
            }
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
            fn checked(n: Int) -> Result(Int, String) {
              if n > 0 { Ok(n) } else { Err("bad") }
            }
            fn main(console: Console) {
              print(console, to_string(result.is_err(checked(5))))
              print(console, to_string(result.is_err(checked(0 - 1))))
              let chained = result.and_then(checked(5), fn(n: Int) { Ok(n * 10) })
              print(console, int_to_string(result.unwrap_or(chained, 0)))
              let mapped = result.map_err(checked(0 - 1), fn(s: String) { string_length(s) })
              print(console, to_string(result.is_err(mapped)))
            }
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
            fn main() -> Int {
              let xs = [1, 2, 3, 4, 5]
              let doubled = list.map(xs, fn(n: Int) { n * 2 })
              let evens = list.filter(xs, fn(n: Int) { n % 2 == 0 })
              let sum = list.fold(doubled, 0, fn(acc: Int, n: Int) { acc + n })
              sum + length(evens)
            }
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
            fn main() -> Int {
              let xs = [3, 1, 4, 1, 5, 9, 2, 6]
              let sorted = list.sort_by(xs, fn(a: Int, b: Int) { a < b })
              at(sorted, 0) * 100 + at(sorted, 7)
            }
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
            fn main() -> Int {
              let a = math.gcd(48, 36)
              let b = math.pow(2, 10)
              let c = math.clamp(15, 0, 10)
              let f = math.fclamp(15.0, 0.0, 10.0)
              let g = math.fabs(0.0 - 3.5)
              a + b + c + float_to_int(f) + float_to_int(g)
            }
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
            fn main() -> Float {
              let a = 10.0 / 4.0
              let b = math.fabs(0.0 - 1.5)
              let c = math.fmax(a, b)
              let d = if c > 2.0 { c * 2.0 } else { 0.0 }
              d - sqrt(4.0)
            }
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
            fn main() -> Float {
              math.fmin(2.5, sqrt(2.25)) + math.fclamp(5.0, 0.0, 1.0)
            }
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
            fn main(console: Console) {
              print(console, int_to_string(math.factorial(5)))
              print(console, int_to_string(math.factorial(0)))
              print(console, int_to_string(math.factorial(1)))
              print(console, to_string(math.is_prime(7)))
              print(console, to_string(math.is_prime(12)))
              print(console, to_string(math.is_prime(1)))
              print(console, to_string(math.is_prime(2)))
              print(console, to_string(math.is_prime(97)))
            }
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
            fn main(console: Console) {
              print(console, int_to_string(math.lcm(4, 6)))
              print(console, int_to_string(math.lcm(21, 6)))
              print(console, int_to_string(math.lcm(0, 5)))
              print(console, int_to_string(math.lcm(0 - 4, 6)))
              print(console, to_string(math.is_even(10)))
              print(console, to_string(math.is_odd(7)))
              print(console, to_string(math.is_odd(0 - 3)))
            }
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
            fn main() -> Int {
              let r = result.map_ok(Ok(20), fn(n: Int) { n + 1 })
              result.unwrap_or(r, 0)
            }
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
            fn main() -> Int {
              let o = option.map(Some(20), fn(n: Int) { n * 2 })
              option.unwrap_or(o, 0)
            }
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
            fn main(console: Console) {
              let p = split("a,bb,ccc", ",")
              print(console, int_to_string(length(p)))         // 3
              print(console, at(p, 0))                          // a
              print(console, at(p, 2))                          // ccc
              print(console, int_to_string(length(split("a,,b", ","))))   // 3
              print(console, at(split("a,,b", ","), 1))         // (empty)
              print(console, int_to_string(length(split("", ","))))       // 1
              print(console, int_to_string(length(split("abc", ""))))     // 1 (empty sep)
              print(console, at(split("xXXyXXz", "XX"), 2))     // z
            }
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
            fn classify(n: Int) -> Bool { n > 0 }
            fn main(console: Console) {
              print(console, to_string(42))
              print(console, to_string(0 - 5))
              print(console, to_string(true))
              print(console, to_string(3 > 7))
              print(console, to_string("hi"))
              print(console, to_string(classify(9)))
              let flag = 2 == 2
              print(console, to_string(flag))
            }
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
            fn apply(f: fn(String) -> String, s: String) -> String { f(s) }
            fn main(console: Console) {
              let x = 5
              print(console, to_string(x))
              print(console, apply(fn(x: String) { to_string(x) }, "hey"))
            }
        "#;
        assert_eq!(run_on_wasm(src), vec!["5", "hey"]);
    }

    #[test]
    fn to_string_on_undetermined_type_is_rejected() {
        // A value type codegen can't pin down (here a list) errors clearly rather
        // than silently mis-rendering.
        let src = r#"
            fn main(console: Console) {
              print(console, to_string([1, 2, 3]))
            }
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
            fn main(console: Console) {
              print(console, int_to_string(0 - 1))
              print(console, int_to_string(0 - 128))
              print(console, int_to_string(255))
              print(console, int_to_string(0))
            }
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
            fn main(console: Console) {
              print(console, replace("a,b,c", ",", ";"))
              print(console, replace("aXXbXXc", "XX", "-"))
              print(console, replace("aaa", "aa", "x"))
              print(console, replace("a,b,c", ",", ""))
              print(console, replace("abc", "b", "XYZ"))
              print(console, replace("abc", "z", "Q"))
              print(console, replace("ab", "", "-"))
              print(console, replace("café", "é", "e"))
            }
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
            fn main(console: Console) {
              print(console, int_to_string(if contains("hello world", "world") { 1 } else { 0 }))
              print(console, int_to_string(if contains("abc", "xyz") { 1 } else { 0 }))
              print(console, int_to_string(if contains("abc", "") { 1 } else { 0 }))
              print(console, int_to_string(index_of("hello", "l")))
              print(console, int_to_string(index_of("hello", "z")))
              print(console, substring("hello", 1, 4))
              print(console, substring("hi", 0, 100))
              print(console, substring("hi", 5, 10))
              print(console, int_to_string(index_of("café!", "!")))
              print(console, substring("café!", 3, 5))
            }
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
            fn main(console: Console) {
              var d = dict_new()
              d = insert(d, "a", 1)
              d = insert(d, "b", 2)
              d = insert(d, "a", 10)
              print(console, int_to_string(get_or(d, "a", 0)))
              print(console, int_to_string(get_or(d, "b", 0)))
              print(console, int_to_string(get_or(d, "z", 0 - 1)))
              print(console, int_to_string(size(d)))
              print(console, int_to_string(if has(d, "b") { 1 } else { 0 }))
              print(console, int_to_string(if has(d, "q") { 1 } else { 0 }))
            }
        "#;
        assert_eq!(run_on_wasm(src), vec!["10", "2", "-1", "2", "1", "0"]);
    }

    #[test]
    fn dict_int_keys_on_wasm() {
        // Int-keyed Dict: keys compared with i32 equality (mode 0).
        let src = r#"
            fn main(console: Console) {
              var d = dict_new()
              d = insert(d, 1, 100)
              d = insert(d, 2, 200)
              print(console, int_to_string(get_or(d, 1, 0)))
              print(console, int_to_string(get_or(d, 2, 0)))
              print(console, int_to_string(get_or(d, 3, 0 - 1)))
            }
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
            fn main(console: Console) {
              var d = dict_new()
              d = insert(d, [1, 2], 5)
              print(console, int_to_string(size(d)))
            }
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
            type Item { price: Int, qty: Int }
            fn lookup(b: Bool) -> Item {
              if b { Item(3, 10) } else { Item(5, 2) }
            }
            fn main(console: Console) {
              print(console, int_to_string(Item(7, 6).price))
              print(console, int_to_string(lookup(true).qty))
              let items = [Item(1, 2), Item(3, 4)]
              print(console, int_to_string(at(items, 1).qty))
            }
        "#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["7", "10", "4"]);
    }

    #[test]
    fn conditional_record_field_access_backends_agree() {
        // `let x = if c { A } else { B }; x.field` (and a match-bound record):
        // the binding's record type is recovered from the branch/arm.
        let src = r#"
            type Item { price: Int, qty: Int }
            fn pick(b: Bool) -> Int {
              let x = if b { Item(3, 10) } else { Item(5, 2) }
              x.price * x.qty
            }
            fn from_tag(t: Int) -> Int {
              let y = match t { 0 -> Item(1, 1)  _ -> Item(2, 3) }
              y.price + y.qty
            }
            fn main(console: Console) {
              print(console, int_to_string(pick(true)))
              print(console, int_to_string(pick(false)))
              print(console, int_to_string(from_tag(0)))
              print(console, int_to_string(from_tag(9)))
            }
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
            type Item { price: Int, qty: Int }
            fn first_value(items: List(Item)) -> Int {
              let first = at(items, 0)
              first.price * first.qty
            }
            fn main(console: Console) {
              print(console, int_to_string(first_value([Item(3, 10), Item(5, 2)])))
              let items = [Item(2, 4), Item(7, 1)]
              let second = at(items, 1)
              print(console, int_to_string(second.price + second.qty))
              var total = 0
              for it in items {
                total = total + it.price
              }
              print(console, int_to_string(total))
            }
        "#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["30", "8", "9"]);
    }

    #[test]
    fn dict_of_records_field_access_backends_agree() {
        // Looking a record up in a Dict and accessing its field: the result of
        // get_or carries the default's record type, so `it.price` resolves.
        let src = r#"
            type Item { price: Int, qty: Int }
            fn main(console: Console) {
              var d = dict_new()
              d = insert(d, "apple", Item(3, 10))
              d = insert(d, "bread", Item(2, 5))
              let it = get_or(d, "apple", Item(0, 0))
              print(console, int_to_string(it.price * it.qty))
              let missing = get_or(d, "milk", Item(0, 0))
              print(console, int_to_string(missing.price))
            }
        "#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["30", "0"]);
    }

    #[test]
    fn dict_remove_backends_agree() {
        // `remove` (string and int keys) — present, absent, and the surviving
        // entries — agrees across the interpreter and compiled backends.
        let src = r#"
            fn main(console: Console) {
              var d = dict_new()
              d = insert(d, "a", 1)
              d = insert(d, "b", 2)
              d = insert(d, "c", 3)
              let d2 = remove(d, "b")
              print(console, int_to_string(size(d2)))                         // 2
              print(console, int_to_string(if has(d2, "b") { 1 } else { 0 })) // 0
              print(console, int_to_string(get_or(d2, "a", 0)))               // 1
              print(console, int_to_string(get_or(d2, "c", 0)))               // 3
              let d3 = remove(d, "missing")
              print(console, int_to_string(size(d3)))                         // 3 (unchanged)
              print(console, int_to_string(size(d)))                          // 3 (original intact)
              var nums = dict_new()
              nums = insert(nums, 10, 100)
              nums = insert(nums, 20, 200)
              let nums2 = remove(nums, 10)
              print(console, int_to_string(size(nums2)))                      // 1
              print(console, int_to_string(get_or(nums2, 20, 0)))             // 200
            }
        "#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["2", "0", "1", "3", "3", "3", "1", "200"]);
    }

    #[test]
    fn dict_keys_values_pairs_on_wasm() {
        // keys/values/pairs compiled to WASM: keys -> list of keys, values ->
        // list of values, pairs -> list of (k, v) tuples destructured in a loop.
        let src = r#"
            fn main(console: Console) {
              var d = dict_new()
              d = insert(d, "a", 10)
              d = insert(d, "b", 20)
              d = insert(d, "c", 30)
              var ksum = 0
              for k in keys(d) { ksum = ksum + string_length(k) }
              print(console, int_to_string(ksum))
              var vsum = 0
              for v in values(d) { vsum = vsum + v }
              print(console, int_to_string(vsum))
              var psum = 0
              for entry in pairs(d) {
                let (k, v) = entry
                psum = psum + string_length(k) + v
              }
              print(console, int_to_string(psum))
            }
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
            fn main() -> Int {
              let parts = string.lines("a\nbb\nccc")
              let joined = string.join(parts, "-")
              let r = string.repeat("z", 5)
              length(parts) * 100 + string_length(joined) + string_length(r)
            }
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
            fn main(console: Console) {
              print(console, string.pad_left("42", 5, "0"))
              print(console, string.pad_right("42", 5, "."))
              print(console, string.pad_left("hello", 3, "x"))
              print(console, string.pad_left("ab", 7, "-="))
            }
        "#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "pad diverged between backends");
        assert_eq!(compiled, vec!["00042", "42...", "hello", "-=-=-ab"]);
    }

    #[test]
    fn std_list_partition_unzip_backends_agree() {
        // partition splits by a predicate in one pass; unzip is the inverse of
        // zip. Both return tuples of lists, so this also exercises tuple-valued
        // returns from generic std functions across backends.
        let client = r#"
            import list
            fn main(console: Console) {
              let xs = [1, 2, 3, 4, 5, 6]
              let (evens, odds) = list.partition(xs, fn(n: Int) { n % 2 == 0 })
              print(console, int_to_string(list.sum(evens)))
              print(console, int_to_string(list.sum(odds)))
              let pairs = list.zip([10, 20, 30], [1, 2, 3])
              let (a, b) = list.unzip(pairs)
              print(console, int_to_string(list.sum(a)))
              print(console, int_to_string(list.sum(b)))
            }
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
            fn main(console: Console) {
              print(console, string.strip_prefix("witchy.lang", "witchy."))
              print(console, string.strip_prefix("witchy.lang", "scala."))
              print(console, string.strip_suffix("main.witchy", ".witchy"))
              print(console, string.strip_suffix("main.rs", ".witchy"))
              print(console, string.strip_prefix("abc", "abc"))
            }
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
            fn main() -> Int {
              var out = []
              var i = 0
              while i < 200 {
                out = push(out, i)
                i = i + 1
              }
              var total = 0
              for x in out { total = total + x }
              total
            }
        "#;
        assert_eq!(run_on_wasm(src), vec!["19900"]); // 199*200/2
    }

    #[test]
    fn large_string_concat_grows_memory() {
        // Concatenating a 400-char string one char at a time allocates ~80KB of
        // intermediate strings — past the initial page — and must grow.
        let src = r#"
            fn main() -> Int {
              var s = ""
              var i = 0
              while i < 400 {
                s = s <> "x"
                i = i + 1
              }
              string_length(s)
            }
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
            fn sum(xs: List(Int)) -> Int {
              match xs {
                [] -> 0
                [h, ..t] -> h + sum(t)
              }
            }
            fn main() -> Int { sum([10, 20, 30, 40]) }
        "#;
        assert_eq!(run_on_wasm(src), vec!["100"]);
    }

    #[test]
    fn list_push_and_concat_on_wasm() {
        // Build a list with `push` in a loop, then `concat` — both allocate new
        // lists at runtime. double_all([1,2,3]) = [2,4,6], ++ [100], summed = 112.
        let src = r#"
            fn double_all(xs: List(Int)) -> List(Int) {
              var out = []
              for x in xs {
                out = push(out, x * 2)
              }
              out
            }
            fn main() -> Int {
              let ys = concat(double_all([1, 2, 3]), [100])
              at(ys, 0) + at(ys, 1) + at(ys, 2) + at(ys, 3)
            }
        "#;
        assert_eq!(run_on_wasm(src), vec!["112"]);
    }

    #[test]
    fn for_in_over_list_on_wasm() {
        // `for x in list` compiles to a WASM loop; sum a list = 100.
        let src = r#"
            fn total(xs: List(Int)) -> Int {
              var sum = 0
              for x in xs {
                sum = sum + x
              }
              sum
            }
            fn main() -> Int {
              total([10, 20, 30, 40])
            }
        "#;
        assert_eq!(run_on_wasm(src), vec!["100"]);
    }

    #[test]
    fn tuple_match_patterns_on_wasm() {
        // Tuple patterns in `match` compile to WASM (no tag; element-wise).
        // classify((3,0))=3, classify((0,5))=5, classify((2,4))=6; sum = 14.
        let src = r#"
            fn classify(p: (Int, Int)) -> Int {
              match p {
                (0, 0) -> 0
                (x, 0) -> x
                (0, y) -> y
                (x, y) -> x + y
              }
            }
            fn main() -> Int {
              classify((3, 0)) + classify((0, 5)) + classify((2, 4))
            }
        "#;
        assert_eq!(run_on_wasm(src), vec!["14"]);
    }

    #[test]
    fn tuple_construct_and_destructure_on_wasm() {
        // Multiple-return-value tuples compile to WASM: divmod(17,5) = (3,2),
        // then 3*100 + 2 = 302.
        let src = r#"
            fn divmod(a: Int, b: Int) -> (Int, Int) {
              (a / b, a % b)
            }
            fn main() -> Int {
              let (q, r) = divmod(17, 5)
              q * 100 + r
            }
        "#;
        assert_eq!(run_on_wasm(src), vec!["302"]);
    }

    #[test]
    fn string_prefix_suffix_on_wasm() {
        // starts_with / ends_with compile to byte-loop helpers.
        // check("html")=2, check("http")=1, check("xml")=0 -> 210.
        let src = r#"
            fn check(s: String) -> Int {
              if starts_with(s, "ht") {
                if ends_with(s, "ml") { 2 } else { 1 }
              } else {
                0
              }
            }
            fn main() -> Int {
              check("html") * 100 + check("http") * 10 + check("xml")
            }
        "#;
        assert_eq!(run_on_wasm(src), vec!["210"]);
    }

    #[test]
    fn try_operator_runs_on_wasm() {
        // `?` compiles: success unwraps, error early-returns. compute(3,4)=Ok(7),
        // compute(0,9)=Err(99); 7*100 + 99 = 799.
        let src = r#"
            type Result { Ok(a) Err(e) }
            fn checked(n: Int) -> Result(Int, Int) {
              match n { 0 -> Err(99)  _ -> Ok(n) }
            }
            fn compute(a: Int, b: Int) -> Result(Int, Int) {
              let x = checked(a)?
              let y = checked(b)?
              Ok(x + y)
            }
            fn main() -> Int {
              // `match` directly on a call result (scrutinee need not be a var).
              let ok = match compute(3, 4) { Ok(v) -> v  Err(e) -> e }
              let bad = match compute(0, 9) { Ok(v) -> v  Err(e) -> e }
              ok * 100 + bad
            }
        "#;
        assert_eq!(run_on_wasm(src), vec!["799"]);
    }

    #[test]
    fn early_return_runs_on_wasm() {
        // Guard-clause early returns compile to valid WASM and run.
        // classify(-5) = -1, classify(0) = 0, classify(9) = 1; sum = 0.
        let src = r#"
            fn classify(n: Int) -> Int {
              if n < 0 { return 0 - 1 }
              if n == 0 { return 0 }
              1
            }
            fn main() -> Int {
              classify(0 - 5) + classify(0) + classify(9)
            }
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
            type Inner { v: Int }
            type Outer { inner: Inner, tag: Int }
            fn deep(o: Outer) -> Int { o.inner.v + o.tag }
            fn main() -> Int {
              let o = Outer(Inner(42), 8)
              deep(o)
            }
        "#;
        assert_eq!(run_on_wasm(src), vec!["50"]);
    }

    #[test]
    fn record_call_and_update_results_field_access_on_wasm() {
        // Field access on a `let` bound to a record-returning call / update —
        // exercises return-record and update-result type tracking in codegen.
        let src = r#"
            type Point { x: Int, y: Int }
            fn make(a: Int, b: Int) -> Point { Point(a, b) }
            fn shift(p: Point, dx: Int) -> Point { update p { x: p.x + dx } }
            fn main() -> Int {
              let p = make(3, 4)
              let q = shift(p, 7)
              q.x + q.y
            }
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
            type Item { price: Int, qty: Int }
            fn total(items: List(Item)) -> Int {
              var sum = 0
              for it in items {
                sum = sum + it.price * it.qty
              }
              sum
            }
            fn main() -> Int { total([Item(3, 2), Item(5, 1)]) }
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
            fn main(console: Console, net: Net) {
              send_line(connect(net, "evil.test:80"), "x")
            }
        "#;
        let e = interpreter::run_with(connect_denied, ".", vec!["allowed.test:80".into()])
            .unwrap_err()
            .to_string();
        assert!(e.contains("not permitted"), "expected a connect denial, got: {e}");

        // restrict to an address not already held is denied (can't widen).
        let restrict_denied = r#"
            fn main(console: Console, net: Net) {
              send_line(connect(restrict(net, "evil.test:80"), "evil.test:80"), "x")
            }
        "#;
        let e = interpreter::run_with(restrict_denied, ".", vec!["allowed.test:80".into()])
            .unwrap_err()
            .to_string();
        assert!(e.contains("not in this Net"), "expected a restrict denial, got: {e}");

        // Attenuation is real: after restricting to one address, a sibling that
        // was in the original grant is no longer reachable.
        let attenuated = r#"
            fn main(console: Console, net: Net) {
              let narrow = restrict(net, "a.test:80")
              send_line(connect(narrow, "b.test:80"), "x")
            }
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

    /// The list-combinator pipeline example runs via the CLI (interpreter); a
    /// companion compiled test (`list_pipeline_example_runs_on_wasm`) asserts the
    /// same output through the WASM backend.
    #[test]
    fn list_pipeline_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/list_pipeline.witchy").unwrap(),
            vec!["233", "2 8", "735"]
        );
    }

    /// `zip`/`enumerate` and tuple destructuring in a loop, via the CLI.
    #[test]
    fn zip_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/zip.witchy").unwrap(),
            vec!["0:alice 1:bob 2:carol", "alice=30 bob=25 carol=40"]
        );
    }

    /// `any`/`all` predicate combinators via the CLI.
    #[test]
    fn predicates_run_via_cli() {
        assert_eq!(
            crate::execute_file("examples/predicates.witchy").unwrap(),
            vec!["true", "true", "false", "false"]
        );
    }

    /// `all` is vacuously true on the empty list; `any` is false.
    #[test]
    fn any_all_empty_list_edge_cases() {
        let client = r#"
            import list
            fn main(console: Console) {
              let empty = list.filter([1], fn(n: Int) { n > 100 })
              print(console, to_string(list.all(empty, fn(n: Int) { n > 0 })))
              print(console, to_string(list.any(empty, fn(n: Int) { n > 0 })))
            }
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
            fn main(console: Console) {
              let ps = list.zip([1, 2, 3], ["a", "b"])
              print(console, int_to_string(length(ps)))
              let first = at(ps, 0)
              let (n, s) = first
              print(console, int_to_string(n) <> s)
            }
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
    fn string_interpolation_backends_agree() {
        // `${expr}` desugars to `<> to_string(expr) <>`, so interpolation works
        // in both backends: String pass-through, Int/Bool via to_string, embedded
        // calls/arithmetic, `\$` for a literal `$`, and adjacent interpolations.
        let src = r#"
            fn main(console: Console) {
              let name = "witchy"
              let age = 3
              print(console, "hi ${name}, age ${age}")
              print(console, "sum: ${int_to_string(age + 10)}")
              print(console, "flag ${age > 1}")
              print(console, "literal \${x} stays")
              print(console, "${name}${name}")
            }
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
            fn main(console: Console) {
              var fs = []
              for i in [1, 2, 3] {
                fs = push(fs, fn(x: Int) { x + i })
              }
              let f0 = at(fs, 0)
              let f2 = at(fs, 2)
              print(console, int_to_string(f0(10)))
              print(console, int_to_string(f2(10)))
            }
        "#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["11", "13"]);
    }

    #[test]
    fn zero_arg_closures_backends_agree() {
        // Zero-argument closures (incl. capturing ones) compile and run.
        let src = r#"
            fn call0(f: fn() -> Int) -> Int { f() }
            fn main(console: Console) {
              print(console, int_to_string(call0(fn() { 42 })))
              let base = 100
              print(console, int_to_string(call0(fn() { base + 1 })))
            }
        "#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["42", "101"]);
    }

    #[test]
    fn closure_capturing_closure_backends_agree() {
        // A closure that captures another closure and calls it through a
        // higher-order function. Both backends agree.
        let src = r#"
            fn apply(f: fn(Int) -> Int, x: Int) -> Int { f(x) }
            fn main(console: Console) {
              let g = fn(x: Int) { x + 1 }
              let h = fn(y: Int) { apply(g, y) * 2 }
              print(console, int_to_string(apply(h, 5)))
              print(console, int_to_string(apply(h, 20)))
            }
        "#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["12", "42"]); // (5+1)*2, (20+1)*2
    }

    #[test]
    fn compound_assignment_backends_agree() {
        // `x op= e` desugars to `x = x op e`; verify all five ops in both
        // backends, in a loop and a sequence.
        let src = r#"
            fn main(console: Console) {
              var sum = 0
              var i = 0
              while i < 5 {
                sum += i
                i += 1
              }
              print(console, int_to_string(sum))
              var x = 100
              x -= 30
              x *= 2
              x /= 7
              x %= 5
              print(console, int_to_string(x))
            }
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
            fn main(console: Console) {
              print(console, "[" <> replace("abc", "", "-") <> "]")
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
            }
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
            fn main(console: Console) {
              print(console, int_to_string(0 - 7 / 2))
              print(console, int_to_string((0 - 7) % 2))
              print(console, int_to_string(7 / (0 - 2)))
              print(console, int_to_string(7 % (0 - 2)))
              print(console, int_to_string((0 - 7) / (0 - 2)))
              var d = dict_new()
              d = insert(d, "k", 1)
              d = insert(d, "k", 2)
              print(console, int_to_string(get_or(d, "k", 0)))
              print(console, int_to_string(size(d)))
              d = remove(d, "missing")
              print(console, int_to_string(size(d)))
              print(console, int_to_string(get_or(d, "absent", 99)))
            }
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
            type Box {
              f: fn(Int) -> Int
              n: Int
            }
            fn main(console: Console) {
              let fns = [fn(x: Int) { x + 1 }, fn(x: Int) { x * 10 }]
              print(console, int_to_string(at(fns, 0)(5)))
              print(console, int_to_string(at(fns, 1)(5)))
              let pick = true
              print(console, int_to_string((if pick { fn(x: Int) { x + 100 } } else { fn(x: Int) { x } })(7)))
              let b = Box(fn(x: Int) { x * 3 }, 7)
              print(console, int_to_string((b.f)(b.n)))
            }
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
            type Opt {
              Nope
              Yep(Int)
            }
            fn describe(o: Opt) -> String {
              match o {
                Yep(n) if n > 10 -> "big"
                Yep(n) -> "small"
                Nope -> "none"
              }
            }
            fn is_even(n: Int) -> Bool {
              if n == 0 { true } else { is_odd(n - 1) }
            }
            fn is_odd(n: Int) -> Bool {
              if n == 0 { false } else { is_even(n - 1) }
            }
            fn main(console: Console) {
              print(console, describe(Yep(50)))
              print(console, describe(Yep(3)))
              print(console, describe(Nope))
              print(console, int_to_string(if is_even(10) { 1 } else { 0 }))
              print(console, int_to_string(if is_odd(7) { 1 } else { 0 }))
            }
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
            fn classify(n: Int) -> String {
              match n {
                x if x < 0 -> "negative"
                0 -> "zero"
                x if x > 100 -> "big"
                _ -> "small"
              }
            }
            fn main(console: Console) {
              print(console, classify(0 - 5))
              print(console, classify(0))
              print(console, classify(200))
              print(console, classify(50))
              print(console, classify(100))
            }
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
            type Point { x: Int  y: Int }
            type Line { from: Point  to: Point }
            fn main(console: Console) {
              let l = Line(Point(1, 2), Point(3, 4))
              print(console, int_to_string(l.from.x))
              print(console, int_to_string(l.to.y))
              let l2 = update l { from: Point(10, 20) }
              print(console, int_to_string(l2.from.x))
              print(console, int_to_string(l2.to.y))
              print(console, int_to_string(l.from.x))
            }
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
            type Tree {
              Leaf
              Node(Tree, Int, Tree)
            }
            fn sum_tree(t: Tree) -> Int {
              match t {
                Leaf -> 0
                Node(l, v, r) -> sum_tree(l) + v + sum_tree(r)
              }
            }
            fn depth(t: Tree) -> Int {
              match t {
                Leaf -> 0
                Node(l, v, r) -> {
                  let dl = depth(l)
                  let dr = depth(r)
                  1 + if dl > dr { dl } else { dr }
                }
              }
            }
            fn main(console: Console) {
              let t = Node(Node(Leaf, 1, Node(Leaf, 5, Leaf)), 2, Node(Leaf, 3, Leaf))
              print(console, int_to_string(sum_tree(t)))
              print(console, int_to_string(depth(t)))
            }
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
            type Item { name: String  qty: Int }
            fn main(console: Console) {
              let cart = [Item("apple", 3), Item("bread", 1), Item("milk", 2)]
              let multi = [it.name for it in cart if it.qty > 1]
              for n in multi { print(console, n) }
              let qtys = [it.qty * 10 for it in cart]
              for q in qtys { print(console, int_to_string(q)) }
            }
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
            fn main(console: Console) {
              let triples = [(a, b, c) for a in 1..=20 for b in a..=20 for c in b..=20 if a * a + b * b == c * c]
              print(console, int_to_string(length(triples)))
              var total = 0
              for t in triples {
                let (a, b, c) = t
                total = total + c
              }
              print(console, int_to_string(total))
            }
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
            fn main(console: Console) {
              let pairs = [x * 10 + y for x in [1, 2] for y in [3, 4]]
              for p in pairs { print(console, int_to_string(p)) }
              let upper = [x * 10 + y for x in [1, 2, 3] for y in [1, 2, 3] if y > x]
              for p in upper { print(console, int_to_string(p)) }
            }
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
            fn main(console: Console) {
              var total = 0
              for x in [1, 2, 3, 4, 5] {
                match x {
                  3 -> { break }
                  _ -> { total = total + x }
                }
              }
              print(console, int_to_string(total))
              var kept = 0
              for y in [1, 2, 3, 4] {
                match y {
                  2 -> { continue }
                  _ -> 0
                }
                kept = kept + y
              }
              print(console, int_to_string(kept))
            }
        "#;
        assert_eq!(interp(src), run_on_wasm(src), "break/continue in match diverged");
        assert_eq!(run_on_wasm(src), vec!["3", "8"]);
    }

    #[test]
    fn break_continue_backends_agree() {
        let src = r#"
            fn main(console: Console) {
              var sum = 0
              for x in [1, 2, 3, 4, 5, 6, 7, 8] {
                if x > 5 { break }
                if x % 2 == 0 { continue }
                sum = sum + x
              }
              print(console, int_to_string(sum))
              var i = 0
              var found = 0
              while i < 100 {
                i = i + 1
                if i < 10 { continue }
                found = i
                break
              }
              print(console, int_to_string(found))
              var count = 0
              for a in [1, 2, 3] {
                for b in [1, 2, 3] {
                  if b == 2 { break }
                  count = count + 1
                }
              }
              print(console, int_to_string(count))
            }
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
            fn main(console: Console) {
              for i in 1..=5 { print(console, int_to_string(i)) }
              print(console, int_to_string(length(0..=0)))
              print(console, int_to_string(length(5..=2)))
              print(console, int_to_string(length([n for n in 1..=4])))
            }
        "#;
        assert_eq!(interp(src), run_on_wasm(src), "inclusive range diverged");
        assert_eq!(run_on_wasm(src), vec!["1", "2", "3", "4", "5", "1", "0", "4"]);
    }

    #[test]
    fn range_operator_backends_agree() {
        let src = r#"
            fn main(console: Console) {
              for i in 0..5 { print(console, int_to_string(i)) }
              let squares = [x * x for x in 1..5]
              for s in squares { print(console, int_to_string(s)) }
              print(console, int_to_string(length(3..3)))
              print(console, int_to_string(length(2..(1 + 4))))
            }
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
            fn main(console: Console) {
              let squares = [n * n for n in [1, 2, 3, 4]]
              for s in squares { print(console, int_to_string(s)) }
              let evens = [n for n in [1, 2, 3, 4, 5, 6] if n % 2 == 0]
              for e in evens { print(console, int_to_string(e)) }
              print(console, int_to_string(length([x for x in [] if x > 0])))
            }
        "#;
        assert_eq!(interp(src), run_on_wasm(src), "list comprehension diverged");
        assert_eq!(run_on_wasm(src), vec!["1", "4", "9", "16", "2", "4", "6", "0"]);
    }

    #[test]
    fn tuple_patterns_backends_agree() {
        let src = r#"
            fn quadrant(x: Int, y: Int) -> String {
              match (x, y) {
                (0, 0) -> "origin"
                (0, _) -> "y-axis"
                (_, 0) -> "x-axis"
                _ -> "other"
              }
            }
            fn describe(pair: (Int, String)) -> String {
              match pair {
                (0, s) -> "zero:" <> s
                (n, "stop") -> "stop@" <> int_to_string(n)
                (n, s) -> s <> "=" <> int_to_string(n)
              }
            }
            fn main(console: Console) {
              print(console, quadrant(0, 0))
              print(console, quadrant(0, 5))
              print(console, quadrant(5, 0))
              print(console, quadrant(2, 3))
              print(console, describe((0, "x")))
              print(console, describe((7, "stop")))
              print(console, describe((4, "k")))
            }
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
            fn main(console: Console) {
              var fns = []
              var i = 0
              while i < 3 {
                let captured = i
                fns = push(fns, fn(x: Int) { x + captured })
                i = i + 1
              }
              for f in fns {
                print(console, int_to_string(f(10)))
              }
            }
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
            fn pair_up(x: a, y: b) -> (a, b) { (x, y) }
            fn first_of(p: (a, b)) -> a {
              let (f, s) = p
              f
            }
            fn second_of(p: (a, b)) -> b {
              let (f, s) = p
              s
            }
            fn main(console: Console) {
              let pi = pair_up(1, 2)
              let ps = pair_up("a", "b")
              let pm = pair_up(7, "mixed")
              print(console, int_to_string(first_of(pi)))
              print(console, first_of(ps))
              print(console, second_of(ps))
              print(console, int_to_string(first_of(pm)))
              print(console, second_of(pm))
            }
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
            type Point { x: Int  y: Int }
            type Line { from: Point  to: Point }
            fn main(console: Console) {
              let l = Line(Point(1, 2), Point(3, 4))
              let p2 = update l.from { x: 100 }
              print(console, int_to_string(p2.x))
              print(console, int_to_string(p2.y))
              let cond = true
              let p3 = update (if cond { l.from } else { l.to }) { y: 99 }
              print(console, int_to_string(p3.x))
              print(console, int_to_string(p3.y))
              let l2 = update l { from: update l.to { x: 7 } }
              print(console, int_to_string(l2.from.x))
              print(console, int_to_string(l2.from.y))
            }
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
            type P { x: Int  y: Int }
            fn mk() -> List(P) { [P(1, 2), P(3, 4), P(5, 6)] }
            fn main(console: Console) {
              for p in mk() {
                print(console, int_to_string(p.x + p.y))
              }
              for q in [P(10, 1), P(20, 2)] {
                print(console, int_to_string(q.x))
              }
            }
        "#;
        assert_eq!(interp(src), run_on_wasm(src), "for over non-var record list diverged");
        assert_eq!(run_on_wasm(src), vec!["3", "7", "11", "10", "20"]);
    }

    #[test]
    fn function_pipeline_fold_backends_agree() {
        let client = r#"
            import list
            fn main(console: Console) {
              let inc = fn(x: Int) { x + 1 }
              let dbl = fn(x: Int) { x * 2 }
              let neg = fn(x: Int) { 0 - x }
              let pipeline = [inc, dbl, neg]
              let result = list.fold(pipeline, 5, fn(acc: Int, f: fn(Int) -> Int) { f(acc) })
              print(console, int_to_string(result))
            }
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
            fn main(console: Console) {
              print(console, int_to_string(char_count("hello")))
              print(console, int_to_string(string_length("hello")))
              print(console, int_to_string(char_count("café")))
              print(console, int_to_string(string_length("café")))
              print(console, int_to_string(char_count("")))
            }
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
            fn main(console: Console) {
              print(console, string.take("hello", 3))
              print(console, "[" <> string.take("hi", 10) <> "]")
              print(console, "[" <> string.take("hi", 0) <> "]")
              print(console, string.drop("hello", 2))
              print(console, "[" <> string.drop("hi", 5) <> "]")
              print(console, string.take("café", 2))
              print(console, string.drop("café", 3))
            }
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
            fn main(console: Console) {
              print(console, string.reverse("hello"))
              print(console, "[" <> string.reverse("") <> "]")
              print(console, string.reverse("a"))
              print(console, string.reverse("café"))
            }
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
            fn main(console: Console) {
              print(console, string.replace_first("a.b.c", ".", "/"))
              print(console, string.replace_first("hello", "l", "L"))
              print(console, string.replace_first("xyz", "q", "Q"))
              print(console, string.replace_first("aa", "a", "bb"))
            }
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
            fn main(console: Console) {
              let (k, v) = string.split_once("name=witchy", "=")
              print(console, k)
              print(console, v)
              let (a, b) = string.split_once("no-sep-here", "=")
              print(console, a)
              print(console, "[" <> b <> "]")
              let (h, rest) = string.split_once("a=b=c", "=")
              print(console, h)
              print(console, rest)
            }
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
            fn main(console: Console) {
              let ws = string.words("the  quick\tbrown\nfox ")
              print(console, int_to_string(length(ws)))
              for w in ws { print(console, w) }
              print(console, int_to_string(length(string.words("   "))))
              print(console, int_to_string(length(string.words(""))))
            }
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
            fn main(console: Console) {
              let cs = string.to_chars("café")
              print(console, int_to_string(length(cs)))
              for c in cs { print(console, c) }
              print(console, int_to_string(length(string.to_chars(""))))
            }
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
            fn main(console: Console) {
              print(console, to_string(string.is_empty("")))
              print(console, to_string(string.is_empty("x")))
              print(console, int_to_string(string.count("banana", "a")))
              print(console, int_to_string(string.count("banana", "an")))
              print(console, int_to_string(string.count("aaaa", "aa")))
              print(console, int_to_string(string.count("abc", "x")))
              print(console, int_to_string(string.count("abc", "")))
            }
        "#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "string is_empty/count diverged");
        assert_eq!(compiled, vec!["true", "false", "3", "2", "2", "0", "0"]);
    }

    #[test]
    fn std_string_char_at_backends_agree() {
        // char_at returns the single character at an index, or "" out of range.
        let client = r#"
            import string
            fn main(console: Console) {
              print(console, string.char_at("witchy", 0))
              print(console, string.char_at("witchy", 5))
              print(console, "[" <> string.char_at("witchy", 10) <> "]")
              print(console, "[" <> string.char_at("", 0) <> "]")
            }
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
            fn put(d: Dict(String, Int), k: String, v: Int) -> Dict(String, Int) {
              insert(d, k, v)
            }
            fn lookup(d: Dict(String, Int), k: String) -> Int {
              get_or(d, k, 0 - 1)
            }
            fn main(console: Console) {
              var d = dict_new()
              d = put(d, "apple", 1)
              d = put(d, "banana", 2)
              print(console, int_to_string(lookup(d, "ap" <> "ple")))
              print(console, int_to_string(lookup(d, "banana")))
              print(console, int_to_string(lookup(d, "cherry")))
            }
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
            fn size(n: Int) -> Int { n * 100 }
            fn main(console: Console) {
              var size = 3
              size = size + 4
              print(console, int_to_string(size))
            }
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
            fn main(console: Console) {
              print(console, int_to_string(list.sum(list.range(5))))
            }
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
            fn main(console: Console) {
              print(console, int_to_string(result.unwrap_or(option.ok_or(Some(5), "none"), 0)))
              print(console, to_string(result.is_err(option.ok_or(None, "none"))))
              print(console, int_to_string(result.unwrap_or(option.ok_or_else(Some(9), fn() { "none" }), 0)))
              print(console, to_string(result.is_err(option.ok_or_else(None, fn() { "none" }))))
            }
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
            fn nested(n: Int) -> Result(Result(Int, String), String) {
              if n > 0 { Ok(Ok(n)) } else { Ok(Err("inner")) }
            }
            fn main(console: Console) {
              print(console, int_to_string(result.unwrap_or(result.flatten(nested(5)), 0)))
              print(console, to_string(result.is_err(result.flatten(nested(0)))))
            }
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
            fn check(n: Int) -> Result(Int, String) {
              if n > 0 { Ok(n) } else { Err("bad") }
            }
            fn main(console: Console) {
              print(console, int_to_string(option.unwrap_or(result.ok(check(5)), 0)))
              print(console, to_string(option.is_none(result.ok(check(0 - 1)))))
              print(console, to_string(option.is_none(result.err(check(5)))))
              print(console, int_to_string(string_length(option.unwrap_or(result.err(check(0 - 1)), ""))))
            }
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
            fn main(console: Console) {
              let s = list.sort([3, 1, 4, 1, 5, 9, 2, 6])
              for x in s { print(console, int_to_string(x)) }
              let u = list.unique([1, 2, 2, 3, 1, 4, 3])
              print(console, int_to_string(length(u)))
              for x in u { print(console, int_to_string(x)) }
            }
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
            type Account { id: Int  balance: Int }
            type P { x: Int  y: Int }
            fn lookup(n: Int) -> Option(Account) {
              if n > 0 { Some(Account(n, n * 100)) } else { None }
            }
            fn mk() -> List(P) { [P(1, 2), P(3, 4)] }
            fn main(console: Console) {
              let o = lookup(7)
              match o {
                Some(a) -> print(console, int_to_string(a.balance))
                None -> print(console, "none")
              }
              let xs = mk()
              for p in xs {
                print(console, int_to_string(p.x))
              }
            }
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
            type Account { id: Int  balance: Int }
            fn lookup(n: Int) -> Result(Account, String) {
              if n > 0 { Ok(Account(n, n * 100)) } else { Err("bad") }
            }
            fn process(n: Int) -> Result(Int, String) {
              let acc = lookup(n)?
              Ok(acc.balance + 1)
            }
            fn main(console: Console) {
              match process(5) {
                Ok(v) -> print(console, int_to_string(v))
                Err(e) -> print(console, e)
              }
              match process(0 - 1) {
                Ok(v) -> print(console, int_to_string(v))
                Err(e) -> print(console, e)
              }
            }
        "#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "? with Result(Record) diverged");
        assert_eq!(compiled, vec!["501", "bad"]);
    }

    #[test]
    fn order_processing_integration_backends_agree() {
        let client = r#"
            import list
            import option
            type Item { name: String  price: Int  qty: Int }
            fn line_total(it: Item) -> Int { it.price * it.qty }
            fn main(console: Console) {
              let cart = [Item("apple", 50, 3), Item("bread", 200, 1), Item("milk", 150, 2)]
              let total = list.fold(cart, 0, fn(acc: Int, it: Item) { acc + line_total(it) })
              print(console, int_to_string(total))
              match list.max_by(cart, fn(a: Item, b: Item) { line_total(a) < line_total(b) }) {
                Some(it) -> print(console, it.name)
                None -> print(console, "none")
              }
              let multi = list.filter(cart, fn(it: Item) { it.qty > 1 })
              for it in multi { print(console, it.name) }
              match list.find(cart, fn(it: Item) { it.name == "bread" }) {
                Some(it) -> print(console, int_to_string(it.price))
                None -> print(console, "0")
              }
            }
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
            type Raw { a: Int  b: Int }
            type Point { x: Int  y: Int }
            fn main(console: Console) {
              let raws = [Raw(1, 2), Raw(3, 4)]
              let pts = list.map(raws, fn(r: Raw) { Point(r.a + r.b, r.a * r.b) })
              for p in pts { print(console, int_to_string(p.x)) }
              for p in list.map(raws, fn(r: Raw) { Point(r.b, r.a) }) {
                print(console, int_to_string(p.y))
              }
            }
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
            type P { x: Int  y: Int }
            fn main(console: Console) {
              let ps = [P(1, 10), P(2, 20), P(3, 30)]
              let evens = list.filter(ps, fn(p: P) { p.x % 2 == 0 })
              for p in evens { print(console, int_to_string(p.y)) }
              for p in list.reverse(ps) { print(console, int_to_string(p.x)) }
            }
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
            type Account { id: Int  balance: Int }
            fn main(console: Console) {
              let accounts = [Account(1, 100), Account(2, 200), Account(3, 300)]
              match list.find(accounts, fn(a: Account) { a.balance > 150 }) {
                Some(acc) -> print(console, int_to_string(acc.balance))
                None -> print(console, "none")
              }
              match list.head(accounts) {
                Some(acc) -> print(console, int_to_string(acc.id))
                None -> print(console, "none")
              }
              match list.find(accounts, fn(a: Account) { a.balance > 999 }) {
                Some(acc) -> print(console, int_to_string(acc.id))
                None -> print(console, "none")
              }
            }
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
            type Account { id: Int  balance: Int }
            fn lookup(n: Int) -> Option(Account) {
              if n > 0 { Some(Account(n, n * 100)) } else { None }
            }
            fn main(console: Console) {
              match lookup(5) {
                Some(a) -> print(console, int_to_string(a.balance))
                None -> print(console, "none")
              }
              match lookup(0 - 1) {
                Some(a) -> print(console, int_to_string(a.balance))
                None -> print(console, "none")
              }
            }
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
            type Point { x: Int  y: Int }
            type Shape {
              Circle(Point)
              Origin
            }
            fn f(s: Shape) -> Int {
              match s {
                Circle(Point(x, y)) -> x + y
                Origin -> 0
              }
            }
            fn main(console: Console) {
              print(console, int_to_string(f(Circle(Point(3, 4)))))
              print(console, int_to_string(f(Circle(Point(10, 1)))))
              print(console, int_to_string(f(Origin)))
            }
        "#;
        assert_eq!(interp(src), run_on_wasm(src), "nested constructor pattern diverged");
        assert_eq!(run_on_wasm(src), vec!["7", "11", "0"]);
    }

    #[test]
    fn match_binds_record_field_backends_agree() {
        let src = r#"
            type Point { x: Int  y: Int }
            type Shape {
              Circle(Point)
              Rect(Int, Int)
            }
            fn describe(s: Shape) -> Int {
              match s {
                Circle(c) -> c.x + c.y
                Rect(w, h) -> w * h
              }
            }
            fn main(console: Console) {
              print(console, int_to_string(describe(Circle(Point(3, 4)))))
              print(console, int_to_string(describe(Rect(5, 6))))
            }
        "#;
        assert_eq!(interp(src), run_on_wasm(src), "match record-field bind diverged");
        assert_eq!(run_on_wasm(src), vec!["7", "30"]);
    }

    // find_map searches and transforms in one pass: the first non-None result
    // of f, or None. Here it returns half of the first even number.
    #[test]
    fn std_list_find_map_backends_agree() {
        let client = r#"
            import list
            import option
            fn main(console: Console) {
              let r = list.find_map([3, 5, 8, 10], fn(x: Int) { if x % 2 == 0 { Some(x / 2) } else { None } })
              print(console, int_to_string(option.unwrap_or(r, 0 - 1)))
              let none = list.find_map([1, 3, 5], fn(x: Int) { if x > 100 { Some(x) } else { None } })
              print(console, to_string(option.is_none(none)))
            }
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
            fn main(console: Console) {
              let xs = [3, 1, 4, 1, 5, 9, 2]
              print(console, int_to_string(option.unwrap_or(list.max_by(xs, fn(a: Int, b: Int) { a < b }), 0)))
              print(console, int_to_string(option.unwrap_or(list.min_by(xs, fn(a: Int, b: Int) { a < b }), 0)))
              print(console, int_to_string(option.unwrap_or(list.max_by(xs, fn(a: Int, b: Int) { (0 - a) < (0 - b) }), 0)))
              print(console, to_string(option.is_none(list.max_by([], fn(a: Int, b: Int) { a < b }))))
            }
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
            fn main(console: Console) {
              print(console, int_to_string(option.unwrap_or(list.min([3, 1, 4, 1, 5]), 0)))
              print(console, int_to_string(option.unwrap_or(list.max([3, 1, 4, 1, 5]), 0)))
              print(console, to_string(option.is_none(list.min([]))))
              print(console, int_to_string(option.unwrap_or(list.position([10, 20, 30], 20), 0 - 1)))
              print(console, to_string(option.is_none(list.position([10, 20], 99))))
            }
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
            fn main(console: Console) {
              print(console, int_to_string(option.unwrap_or(list.head([10, 20]), 0)))
              print(console, int_to_string(option.unwrap_or(list.head([]), 0 - 1)))
              print(console, int_to_string(option.unwrap_or(list.last([10, 20]), 0)))
              print(console, int_to_string(option.unwrap_or(list.get([10, 20, 30], 1), 0)))
              print(console, int_to_string(option.unwrap_or(list.get([10], 5), 0 - 1)))
              print(console, int_to_string(option.unwrap_or(list.find([1, 3, 4], fn(n: Int) { n % 2 == 0 }), 0 - 1)))
              print(console, to_string(option.is_none(list.find([1, 3, 5], fn(n: Int) { n % 2 == 0 }))))
            }
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
            fn main(console: Console) {
              print(console, int_to_string(list.head_or([10, 20, 30], 0)))
              print(console, int_to_string(list.head_or([], 0 - 1)))
              print(console, int_to_string(list.last_or([10, 20, 30], 0)))
              print(console, int_to_string(list.last_or([], 0 - 1)))
              print(console, int_to_string(list.find_or([1, 3, 4, 7], fn(n: Int) { n % 2 == 0 }, 0 - 1)))
              print(console, int_to_string(list.find_or([1, 3, 5], fn(n: Int) { n % 2 == 0 }, 0 - 1)))
            }
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
            fn main(console: Console) {
              let ws = list.windows([1, 2, 3, 4], 2)
              print(console, int_to_string(length(ws)))
              for w in ws { print(console, int_to_string(list.sum(w))) }
              print(console, int_to_string(length(list.windows([1, 2], 5))))
              print(console, int_to_string(length(list.windows([1, 2, 3], 0))))
            }
        "#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "windows diverged");
        assert_eq!(compiled, vec!["3", "3", "5", "7", "0", "0"]);
    }

    #[test]
    fn std_list_chunks_tail_init_backends_agree() {
        // chunks groups into fixed-size sublists (last may be short), tail drops
        // the first element, init drops the last — all total (empty stays empty).
        // Iterating List(List(Int)) also exercises nested lists across backends.
        let client = r#"
            import list
            fn main(console: Console) {
              let cs = list.chunks([1, 2, 3, 4, 5], 2)
              print(console, int_to_string(length(cs)))
              for c in cs { print(console, int_to_string(list.sum(c))) }
              print(console, int_to_string(list.sum(list.tail([1, 2, 3]))))
              print(console, int_to_string(list.sum(list.init([1, 2, 3]))))
              print(console, int_to_string(length(list.tail([]))))
              print(console, int_to_string(length(list.init([]))))
            }
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
            type Item { price: Int  qty: Int }
            fn main(console: Console) {
              let cart = [Item(50, 3), Item(200, 1), Item(150, 2)]
              print(console, int_to_string(list.sum_by(cart, fn(it: Item) { it.price * it.qty })))
              print(console, int_to_string(list.sum_by([1, 2, 3, 4], fn(n: Int) { n * n })))
              print(console, int_to_string(list.sum_by([], fn(n: Int) { n })))
            }
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
            fn main(console: Console) {
              print(console, int_to_string(list.product([1, 2, 3, 4])))
              print(console, int_to_string(list.product([])))
              let s = list.slice([10, 20, 30, 40, 50], 1, 4)
              for x in s { print(console, int_to_string(x)) }
              let running = list.scan([1, 2, 3], 0, fn(acc: Int, n: Int) { acc + n })
              for x in running { print(console, int_to_string(x)) }
            }
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
            fn double(x: Int) -> Int { x * 2 }
            fn inc(x: Int) -> Int { x + 1 }
            fn sub(a: Int, b: Int) -> Int { a - b }
            fn main(console: Console) {
              let h = func.compose(double, inc)
              print(console, int_to_string(h(10)))
              print(console, int_to_string(func.flip(sub)(3, 10)))
              print(console, int_to_string(func.constant(42)(999)))
              print(console, int_to_string(func.identity(7)))
            }
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
            fn compose(f: fn(Int) -> Int, g: fn(Int) -> Int) -> fn(Int) -> Int {
              fn(x: Int) { f(g(x)) }
            }
            fn double(x: Int) -> Int { x * 2 }
            fn inc(x: Int) -> Int { x + 1 }
            fn main(console: Console) {
              let h = compose(double, inc)
              print(console, int_to_string(h(10)))
              print(console, int_to_string(compose(inc, double)(10)))
            }
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
            fn double(x: Int) -> Int { x * 2 }
            fn inc(x: Int) -> Int { x + 1 }
            fn main(console: Console) {
              let f = double
              print(console, int_to_string(f(5)))
              let g = inc
              print(console, int_to_string(g(g(g(0)))))
            }
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
            fn triple(x: Int) -> Int { x * 3 }
            fn main(console: Console) {
              let ys = list.map([1, 2, 3], triple)
              for y in ys { print(console, int_to_string(y)) }
            }
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
            fn twice(f: fn(Int) -> Int, x: Int) -> Int { f(f(x)) }
            fn main(console: Console) {
              let make_adder = fn(x: Int) { fn(y: Int) { x + y } }
              let make_mul = fn(a: Int) { fn(b: Int) { fn(c: Int) { a * b * c } } }
              print(console, int_to_string(make_adder(10)(5)))
              print(console, int_to_string(make_mul(2)(3)(4)))
              print(console, int_to_string((fn(n: Int) { n * n })(7)))
              print(console, int_to_string(twice(make_adder(1), 10)))
              print(console, int_to_string(make_adder(10)(make_adder(2)(3))))
            }
        "#;
        assert_eq!(interp(src), run_on_wasm(src), "immediate application diverged");
        assert_eq!(run_on_wasm(src), vec!["15", "24", "49", "12", "15"]);
    }

    #[test]
    fn closures_and_string_ordering_backends_agree() {
        let src = r#"
            fn main(console: Console) {
              let base = 10
              let add = fn(n: Int) { n + base }
              var total = 0
              var i = 0
              while i < 5 {
                total = total + add(i)
                i = i + 1
              }
              print(console, int_to_string(total))
              let make_adder = fn(x: Int) { fn(y: Int) { x + y } }
              let add3 = make_adder(3)
              print(console, int_to_string(add3(4)))
              print(console, int_to_string(make_adder(100)(1)))
              if "abc" < "abcd" { print(console, "lt1") } else { print(console, "ge1") }
              if "Z" < "a" { print(console, "lt2") } else { print(console, "ge2") }
              if "" < "a" { print(console, "lt3") } else { print(console, "ge3") }
              if "apple" < "apply" { print(console, "lt4") } else { print(console, "ge4") }
            }
        "#;
        assert_eq!(interp(src), run_on_wasm(src), "closures/ordering diverged");
    }

    #[test]
    fn string_edge_cases_backends_agree() {
        let src = r#"
            fn main(console: Console) {
              print(console, int_to_string(length(split("abc", ""))))
              print(console, int_to_string(length(split("abc", "x"))))
              print(console, int_to_string(length(split("a,b,c", ","))))
              print(console, "[" <> substring("", 0, 5) <> "]")
              print(console, "[" <> substring("hello", 3, 1) <> "]")
              print(console, substring("hello", 2, 100))
              print(console, int_to_string(index_of("hello", "")))
              print(console, int_to_string(index_of("hello", "z")))
              print(console, "[" <> ("" <> "x" <> "") <> "]")
              print(console, int_to_string(string_length("")))
            }
        "#;
        assert_eq!(interp(src), run_on_wasm(src), "string edge cases diverged");
    }

    #[test]
    fn trim_backends_agree() {
        // trim now compiles: leading/trailing ASCII whitespace (spaces, tabs,
        // newlines, CRs) is stripped; an all-whitespace string trims to "".
        let src = r#"
            fn main(console: Console) {
              print(console, trim("  hello  "))
              print(console, trim("\t\nfoo\r\n"))
              print(console, trim("nospaces"))
              print(console, trim("   "))
              print(console, int_to_string(string_length(trim("  a b  "))))
            }
        "#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["hello", "foo", "nospaces", "", "3"]);
    }

    #[test]
    fn string_to_int_backends_agree() {
        // string_to_int now compiles: leading whitespace and an optional sign
        // are honored, and the parsed value feeds straight into arithmetic.
        let src = r#"
            fn main(console: Console) {
              print(console, int_to_string(string_to_int("42")))
              print(console, int_to_string(string_to_int("-17")))
              print(console, int_to_string(string_to_int("  123  ")))
              print(console, int_to_string(string_to_int("+8")))
              print(console, int_to_string(string_to_int("0")))
              print(console, int_to_string(string_to_int("1000000") + 1))
            }
        "#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["42", "-17", "123", "8", "0", "1000001"]);
    }

    #[test]
    fn bitwise_not_backends_agree() {
        // ~x = -x-1 (width-independent), so it agrees across backends.
        let src = r#"
            fn main(console: Console) {
              print(console, int_to_string(~0))
              print(console, int_to_string(~5))
              print(console, int_to_string(~(0 - 1)))
              print(console, int_to_string(255 & ~15))
            }
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
            fn classify(n: Int) -> String {
              match n { 1 | 2 | 4 -> "pow2"  _ -> "other" }
            }
            fn main(console: Console) {
              print(console, int_to_string(12 & 10))
              print(console, int_to_string(12 | 10))
              print(console, int_to_string(12 ^ 10))
              print(console, int_to_string(1 << 4))
              print(console, int_to_string(256 >> 2))
              print(console, int_to_string(5 & 3 | 8))
              print(console, to_string(5 & 4 == 4))
              print(console, classify(2))
              print(console, classify(3))
            }
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
            type Shape { Circle(Int)  Square(Int)  Rect(Int, Int) }
            fn classify(n: Int) -> String {
              match n { 1 | 2 | 3 -> "small"  4 | 5 -> "medium"  _ -> "big" }
            }
            fn side(s: Shape) -> Int {
              match s { Circle(r) | Square(r) -> r  Rect(w, h) -> w }
            }
            fn main(console: Console) {
              print(console, classify(2))
              print(console, classify(5))
              print(console, classify(10))
              print(console, int_to_string(side(Circle(5))))
              print(console, int_to_string(side(Square(7))))
              print(console, int_to_string(side(Rect(3, 4))))
            }
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
            fn main(console: Console) {
              let x = 1
              if true {
                let x = 2
                print(console, int_to_string(x))
              }
              print(console, int_to_string(x))
            }
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
            type Bag { items: List(Int), label: String }
            fn main(console: Console) {
              let b = Bag([10, 20, 30], "nums")
              print(console, b.label)
              print(console, int_to_string(length(b.items)))
              var total = 0
              for x in b.items {
                total = total + x
              }
              print(console, int_to_string(total))
              print(console, int_to_string(at(b.items, 1)))
            }
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
            type Inner { v: Int }
            type Outer { name: String, inner: Inner }
            fn main(console: Console) {
              let o = Outer("x", Inner(42))
              print(console, int_to_string(o.inner.v))
              let o2 = update o { inner: Inner(o.inner.v + 1) }
              print(console, int_to_string(o2.inner.v))
              print(console, o.name)
              print(console, int_to_string(o.inner.v))
            }
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
            fn swap(inout a: Int, inout b: Int) {
              let t = a
              a = b
              b = t
            }
            fn bump_by(inout n: Int, d: Int) {
              n = n + d
            }
            fn main(console: Console) {
              var x = 3
              var y = 8
              swap(x, y)
              print(console, int_to_string(x))
              print(console, int_to_string(y))
              var acc = 0
              var i = 1
              while i < 5 {
                bump_by(acc, i)
                i = i + 1
              }
              print(console, int_to_string(acc))
            }
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
