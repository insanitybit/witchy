    use super::*;
    use witchy_syntax::parser::parse_module;
    use std::sync::{Arc, Mutex};
    use wasmtime::{Caller, Engine, Linker, Module as WtModule, Store};

    /// (RFC-0045) Define the always-linked, authority-free `__witchy_abort` import
    /// so a module that routes an abort through it (float ordering, list/bytes OOB,
    /// str_to_int, `fail`) instantiates in these minimal test linkers. The body
    /// traps, matching the real host's `bail!` contract (the call never returns).
    fn define_abort<T: 'static>(linker: &mut Linker<T>) {
        linker
            .func_wrap(
                "witchy",
                "__witchy_abort",
                |_: Caller<'_, T>, _t: i32, _a: i64, _b: i64, _s: i32| -> wasmtime::Result<()> {
                    wasmtime::bail!("runtime error (test harness abort)")
                },
            )
            .unwrap();
    }

    #[test]
    fn build_module_is_zero_ambient() {
        // A compiled build step imports ONLY its build host functions — none of
        // the runtime authority. That's the structural zero-ambient guarantee:
        // the dangerous host functions don't exist for the guest to call.
        let module = parse_module(
            "fn build(out: BuildOut, schema: BuildRead):\n    write_out(out, \"x.witchy\", read_build(schema, \"a.proto\"))\n",
        )
        .expect("parse");
        let wasm = compile_build_module(&module).expect("compile build module");
        let mut imports = Vec::new();
        let mut exports = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(&wasm) {
            match payload.expect("valid wasm") {
                wasmparser::Payload::ImportSection(reader) => {
                    for imp in reader.into_imports() {
                        imports.push(imp.expect("import").name.to_string());
                    }
                }
                wasmparser::Payload::ExportSection(reader) => {
                    for ex in reader {
                        exports.push(ex.expect("export").name.to_string());
                    }
                }
                _ => {}
            }
        }
        assert!(exports.iter().any(|e| e == "run"), "build entrypoint becomes the run export");
        assert!(imports.iter().any(|i| i == "build_out_write"), "write_out import present");
        assert!(imports.iter().any(|i| i == "build_read_len"), "read_build import present");
        // No runtime-authority imports leaked in.
        for forbidden in ["dir_write", "dir_read_len", "net_connect", "net_listen", "print", "now", "now_monotonic", "crypto.sign"] {
            assert!(!imports.iter().any(|i| i == forbidden), "build module must not import `{forbidden}`: {imports:?}");
        }
    }

    #[test]
    fn grantful_build_primitives_compile_to_build_imports_only() {
        let module = parse_module(
            "import option\nfn build(out: BuildOut, env: BuildEnv, dl: BuildNet, cc: BuildExec):\n    let v = match get_build_env(env, \"WITCHY_BUILD_ALLOWED\"):\n        Some(x) -> x\n        None -> \"unset\"\n    write_out(out, \"x.witchy\", v + fetch_build(dl, \"127.0.0.1:9\", \"/schema\") + run_tool(cc, \"cat\", \"input\"))\n",
        )
        .expect("parse");
        let wasm = compile_build_module(&module).expect("compile build module");
        let mut imports = Vec::new();
        for payload in wasmparser::Parser::new(0).parse_all(&wasm) {
            if let wasmparser::Payload::ImportSection(reader) = payload.expect("valid wasm") {
                for imp in reader.into_imports() {
                    imports.push(imp.expect("import").name.to_string());
                }
            }
        }

        for needed in [
            "build_out_write",
            "build_env_len",
            "build_env_fill",
            "build_fetch_len",
            "build_exec_run",
        ] {
            assert!(imports.iter().any(|i| i == needed), "build import `{needed}` missing: {imports:?}");
        }
        for forbidden in ["env_len", "env_fill", "exec_run", "net_connect", "net_try_connect"] {
            assert!(
                !imports.iter().any(|i| i == forbidden),
                "build primitive must not lower to runtime import `{forbidden}`: {imports:?}"
            );
        }
    }

    fn run_int(src: &str) -> i64 {
        let module = parse_module(src).expect("parse");
        let bytes = compile_module_binary(&module)
            .expect("compile")
            .expect("the binary path lowers this program");
        let engine = Engine::default();
        let wt = WtModule::new(&engine, &bytes).expect("valid wasm");
        let captured = Arc::new(Mutex::new(None));
        let mut linker = Linker::new(&engine);
        define_abort(&mut linker);
        let sink = Arc::clone(&captured);
        linker
            .func_wrap("witchy", "print_int", move |n: i64| {
                *sink.lock().unwrap() = Some(n);
            })
            .unwrap();
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &wt).unwrap();
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .unwrap()
            .call(&mut store, ())
            .unwrap();
        captured.lock().unwrap().take().expect("printed a value")
    }

    /// Run a float program with a capturing `print_float`.
    fn run_float(src: &str) -> f64 {
        let module = parse_module(src).expect("parse");
        let bytes = compile_module_binary(&module)
            .expect("compile")
            .expect("the binary path lowers this program");
        let engine = Engine::default();
        let wt = WtModule::new(&engine, &bytes).expect("valid wasm");
        let captured = Arc::new(Mutex::new(None));
        let mut linker = Linker::new(&engine);
        define_abort(&mut linker);
        let sink = Arc::clone(&captured);
        linker
            .func_wrap("witchy", "print_float", move |x: f64| {
                *sink.lock().unwrap() = Some(x);
            })
            .unwrap();
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &wt).unwrap();
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .unwrap()
            .call(&mut store, ())
            .unwrap();
        captured.lock().unwrap().take().expect("printed a float")
    }

    #[test]
    fn compiles_floats() {
        let src = r#"
fn half(x: Float) -> Float:
    (x / 2.0)

fn main() -> Float:
    (half(7.0) + 1.5)
"#;
        assert_eq!(run_float(src), 5.0); // 3.5 + 1.5
    }

    #[test]
    fn float_valued_if_compiles() {
        // An `if/else` whose branches are Float must yield an f64 result (the
        // `if` result type follows the branch kind, not a hardcoded i32).
        let src = r#"
fn pick(a: Float, b: Float) -> Float:
    if (a < b):
        a
    else:
        b

fn main() -> Float:
    (pick(2.5, 7.5) + pick(9.0, 1.0))
"#;
        assert_eq!(run_float(src), 3.5); // min(2.5,7.5)=2.5 + min(9.0,1.0)=1.0
    }

    #[test]
    fn large_int_literals_compile() {
        // Compiled Int is i64, so a literal beyond the 32-bit range round-trips
        // (it no longer wraps or is rejected), matching the interpreter.
        assert_eq!(run_int("fn main() -> Int:\n    3000000000\n"), 3_000_000_000);
        assert_eq!(
            run_int("fn main() -> Int:\n    9000000000000\n"),
            9_000_000_000_000
        );
    }

    #[test]
    fn float_record_field_compiles() {
        // 8-byte heap slots hold an f64 field; float_to_int reads it back.
        let src = r#"
type Vec2:
    x: Float
    y: Float

fn main() -> Int:
    let v = Vec2(1.5, 2.5)
    math.to_int((v).x)
"#;
        assert_eq!(run_int(src), 1);
    }

    #[test]
    fn float_list_element_compiles() {
        // 8-byte heap slots hold an f64, so floats now live in lists.
        let src = r#"
fn main() -> Int:
    let xs = [1.5, 2.5]
    list.length(xs)
"#;
        assert_eq!(run_int(src), 2);
    }

    #[test]
    fn compiles_non_capturing_closure() {
        // A non-capturing lambda passed to a higher-order function: lifted to a
        // table slot, then invoked via `call_indirect`.
        let src = r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main() -> Int:
    apply(fn(n: Int): (n * n), 9)
"#;
        assert_eq!(run_int(src), 81);
    }

    #[test]
    fn compiles_multiple_closures() {
        // Two distinct lambdas take distinct table slots and call_indirect each.
        let src = r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main() -> Int:
    let a = apply(fn(n: Int): (n + 1), 10)
    let b = apply(fn(n: Int): (n * 3), 10)
    (a + b)
"#;
        assert_eq!(run_int(src), 41); // 11 + 30
    }

    #[test]
    fn closure_can_call_global_function() {
        // A lambda calling a top-level function is still non-capturing.
        let src = r#"
fn dbl(x: Int) -> Int:
    (x * 2)

fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main() -> Int:
    apply(fn(n: Int): (dbl(n) + 1), 4)
"#;
        assert_eq!(run_int(src), 9); // dbl(4) + 1
    }

    #[test]
    fn compiles_capturing_closure() {
        // The lambda reads `k` from the enclosing scope: captured by value into
        // the closure's heap environment, then read back via the env prologue.
        let src = r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main() -> Int:
    let k = 100
    apply(fn(n: Int): (n + k), 5)
"#;
        assert_eq!(run_int(src), 105);
    }

    #[test]
    fn closure_captures_multiple_vars() {
        // Several captures land in distinct environment slots.
        let src = r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main() -> Int:
    let a = 3
    let b = 7
    let c = 11
    apply(fn(n: Int): (((n * a) + b) - c), 10)
"#;
        assert_eq!(run_int(src), 26); // 10*3 + 7 - 11
    }

    #[test]
    fn closure_captures_record_field() {
        // Capturing a record value: the env carries the heap pointer, and field
        // access still resolves inside the lambda.
        let src = r#"
type Point:
    x: Int
    y: Int

fn apply(f: fn(Int) -> Int, n: Int) -> Int:
    f(n)

fn main() -> Int:
    let p = Point(4, 9)
    apply(fn(n: Int): (n + ((p).x * (p).y)), 1)
"#;
        assert_eq!(run_int(src), 37); // 1 + 4*9
    }

    #[test]
    fn closure_assigning_captured_var_is_rejected() {
        // By-value capture cannot propagate a write back to the outer binding, so
        // assigning a captured variable is rejected rather than diverging.
        let src = r#"
fn run(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)
fn main() -> Int:
    var total = 0
    let add = fn(n: Int):
        total = total + n
    run(add, 5)
"#;
        let module = parse_module(src).expect("parse");
        let err = compile_module_binary(&module)
            .expect_err("should reject outer assignment");
        assert!(
            err.to_string().contains("assigns `total`"),
            "unexpected error: {err}"
        );
    }

    /// Build a wasmtime instance whose `print` captures strings from memory.
    fn instantiate_with_print(
        bytes: &[u8],
    ) -> (Store<()>, wasmtime::Instance, Arc<Mutex<Vec<String>>>) {
        let engine = Engine::default();
        let wt = WtModule::new(&engine, bytes).expect("valid wasm");
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut linker = Linker::new(&engine);
        define_abort(&mut linker);
        let sink = Arc::clone(&captured);
        linker
            .func_wrap(
                "witchy",
                "print",
                move |mut caller: Caller<'_, ()>, ptr: i32, len: i32| {
                    let mem = caller.get_export("memory").unwrap().into_memory().unwrap();
                    let data = mem.data(&caller);
                    let bytes = &data[ptr as usize..(ptr + len) as usize];
                    sink.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(bytes).into_owned());
                },
            )
            .unwrap();
        let mut store = Store::new(&engine, ());
        let instance = linker.instantiate(&mut store, &wt).unwrap();
        (store, instance, captured)
    }

    fn run_str(src: &str) -> Vec<String> {
        let module = parse_module(src).expect("parse");
        let bytes = compile_module_binary(&module)
            .expect("compile")
            .expect("the binary path lowers this program");
        let (mut store, instance, captured) = instantiate_with_print(&bytes);
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .unwrap()
            .call(&mut store, ())
            .unwrap();
        captured.lock().unwrap().clone()
    }

    #[test]
    fn compiles_arithmetic() {
        assert_eq!(run_int(r#"
fn main() -> Int:
    (1 + (2 * 3))
"#), 7);
    }

    #[test]
    fn full_int_program() {
        let src = r#"
fn double(n: Int) -> Int:
    (n * 2)

fn fib(n: Int) -> Int:
    if (n < 2):
        n
    else:
        (fib((n - 1)) + fib((n - 2)))

fn main() -> Int:
    let a = double(21)
    let b = fib(10)
    (a + b)
"#;
        assert_eq!(run_int(src), 97);
    }

    #[test]
    fn compiles_int_float_conversions() {
        // math.to_float(7) / 2.0 = 3.5; math.to_int(3.5) = 3
        assert_eq!(
            run_int("fn main() -> Int:\n    math.to_int(math.to_float(7) / 2.0)\n"),
            3
        );
    }

    #[test]
    fn compiles_string_length() {
        assert_eq!(run_int(r#"
fn main() -> Int:
    string.length("hello")
"#), 5);
    }

    #[test]
    fn compiles_while_and_mod() {
        // sum of multiples of 3 below 10: 0 + 3 + 6 + 9
        let src = r#"
fn main() -> Int:
    var i = 0
    var total = 0
    while (i < 10):
        if ((i % 3) == 0):
            total = (total + i)
        i = (i + 1)
    total
"#;
        assert_eq!(run_int(src), 18);
    }

    #[test]
    fn compiles_boolean_ops() {
        let src = r#"
fn in_range(n: Int) -> Int:
    if ((n > 0) && (n < 10)):
        1
    else:
        0

fn main() -> Int:
    ((in_range(5) + in_range(50)) + in_range((-3)))
"#;
        assert_eq!(run_int(src), 1); // 1 + 0 + 0
    }

    #[test]
    fn compiles_boolean_not() {
        assert_eq!(run_int("fn main() -> Int:\n    if !(1 == 2): 7 else: 0\n"), 7);
    }

    #[test]
    fn compiles_match_with_guards() {
        let src = r#"
fn sign(n: Int) -> Int:
    match n:
        0 -> 0
        m if (m > 0) -> 1
        _ -> (0 - 1)

fn main() -> Int:
    ((sign(5) + sign((-3))) + sign(0))
"#;
        assert_eq!(run_int(src), 0); // 1 + (-1) + 0
    }

    #[test]
    fn compiles_adts_and_constructor_patterns() {
        // Constructors become heap records [tag][fields...]; ctor patterns load
        // the tag and bind fields.
        let src = r#"
type Shape:
    Circle(Int)
    Square(Int)

fn area(s: Shape) -> Int:
    match s:
        Circle(r) -> ((3 * r) * r)
        Square(w) -> (w * w)

fn main() -> Int:
    (area(Circle(10)) + area(Square(5)))
"#;
        assert_eq!(run_int(src), 325);
    }

    #[test]
    fn renames_calls_to_shadowed_local_closures() {
        // A called LOCAL closure (`f(x)`, where `f` is bound by a match pattern)
        // must keep its call site when alpha-rename gives it a unique name. Both
        // arms bind `f`; the second is renamed so the two don't alias one WASM
        // local, and the body's `f(x)` has to follow that rename. Before the fix
        // the `Call` name was assumed to always be a global, so the renamed local
        // lost its call site and compiled to a trap / unknown-function error —
        // the bug that blocked `chan.address` (Recv + Whoami both bind `cont`).
        let src = r#"
type Box:
    A(fn(Int) -> Int)
    B(fn(Int) -> Int)

fn dbl(n: Int) -> Int:
    (n + n)

fn apply_it(b: Box, x: Int) -> Int:
    match b:
        A(f) -> f(x)
        B(f) -> f(x)

fn main() -> Int:
    (apply_it(A(dbl), 5) + apply_it(B(dbl), 10))
"#;
        assert_eq!(run_int(src), 30);
    }

    #[test]
    fn compiles_lists() {
        let src = r#"
fn main() -> Int:
    let xs = [10, 20, 30]
    ((list.length(xs) + list.at(xs, 0)) + list.at(xs, 2))
"#;
        assert_eq!(run_int(src), 43); // 3 + 10 + 30
    }

    #[test]
    fn compiles_nested_constructor_patterns() {
        let src = r#"
type Point:
    Point(Int, Int)

type Shape:
    Dot(Point)
    Pair(Point, Point)

fn x_of(s: Shape) -> Int:
    match s:
        Dot(Point(x, _)) -> x
        Pair(Point(x, _), _) -> x

fn main() -> Int:
    (x_of(Dot(Point(7, 9))) + x_of(Pair(Point(3, 0), Point(0, 0))))
"#;
        assert_eq!(run_int(src), 10); // 7 + 3
    }

    #[test]
    fn compiles_string_patterns() {
        let src = r#"
fn classify(s: String) -> Int:
    match s:
        "yes" -> 1
        "no" -> 0
        _ -> (0 - 1)

fn main() -> Int:
    ((classify("yes") + classify("no")) + classify("maybe"))
"#;
        assert_eq!(run_int(src), 0); // 1 + 0 + (-1)
    }

    #[test]
    fn compiles_match_and_recursion() {
        let src = r#"
fn fact(n: Int) -> Int:
    match n:
        0 -> 1
        _ -> (n * fact((n - 1)))

fn main() -> Int:
    fact(5)
"#;
        assert_eq!(run_int(src), 120);
    }

    #[test]
    fn compiles_var_writeback() {
        // `var` compiles to move-in / move-out: bump returns the updated n,
        // and the caller writes it back into x.
        let src = r#"
fn bump(var n: Int):
    n = (n + 1)

fn main() -> Int:
    var x = 41
    bump(x)
    bump(x)
    x
"#;
        assert_eq!(run_int(src), 43);
    }

    #[test]
    fn compiles_string_concatenation() {
        let src = r#"
fn shout(name: String) -> String:
    ("hello, " + name)

fn main(console: Console):
    print(console, shout("witchy"))
"#;
        assert_eq!(run_str(src), vec!["hello, witchy"]);
    }

    #[test]
    fn compiles_int_to_string() {
        let src = r#"
fn main(console: Console):
    print(console, __render(12345))
"#;
        assert_eq!(run_str(src), vec!["12345"]);
    }

    #[test]
    fn int_to_string_handles_zero() {
        let src = r#"
fn main(console: Console):
    print(console, __render(0))
"#;
        assert_eq!(run_str(src), vec!["0"]);
    }

    /// (BUG-008) Compile `src` under the optimization set `opt` and report the two
    /// call-SHAPE signals the `direct-call` / `bounds-elide` levers move: the set of
    /// function names reached by a DIRECT `call` and the count of `call_indirect`
    /// operators. Callee indices are resolved through the emitted name section
    /// (imports first, then defined funcs — the order `wir_encode` writes), so a
    /// devirtualized closure call shows up as a direct call to `__lamw{i}` and a
    /// checked list access as a direct call to `list_at`. This inspects the raw
    /// witchy-emitted wasm (`compile_module_binary` runs no Binaryen), so the shape
    /// is the lever's own doing, not a downstream inliner's.
    fn call_shape(
        src: &str,
        opt: witchy_syntax::opt::OptSet,
    ) -> (std::collections::HashSet<String>, usize) {
        use std::collections::{HashMap, HashSet};
        witchy_syntax::opt::set_for_tests(Some(opt));
        let module = parse_module(src).expect("parse");
        let compiled = compile_module_binary(&module);
        witchy_syntax::opt::set_for_tests(None);
        let bytes = compiled
            .expect("compile")
            .expect("the binary path lowers this program");

        let mut names: HashMap<u32, String> = HashMap::new();
        let mut called: Vec<u32> = Vec::new();
        let mut indirect = 0usize;
        for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
            match payload.expect("valid wasm") {
                wasmparser::Payload::CustomSection(reader) => {
                    if let wasmparser::KnownCustom::Name(section) = reader.as_known() {
                        for sub in section {
                            if let wasmparser::Name::Function(map) = sub.expect("name subsection") {
                                for naming in map {
                                    let naming = naming.expect("naming");
                                    names.insert(naming.index, naming.name.to_string());
                                }
                            }
                        }
                    }
                }
                wasmparser::Payload::CodeSectionEntry(body) => {
                    for op in body.get_operators_reader().expect("operators") {
                        match op.expect("operator") {
                            wasmparser::Operator::Call { function_index } => called.push(function_index),
                            wasmparser::Operator::CallIndirect { .. } => indirect += 1,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        let direct: HashSet<String> = called
            .into_iter()
            .map(|i| names.get(&i).cloned().unwrap_or_else(|| format!("#{i}")))
            .collect();
        (direct, indirect)
    }

    #[test]
    fn devirtualizes_single_bound_closure_call() {
        // (RFC-0034 L3 / BUG-008) A closure local bound by exactly one `let` and never
        // reassigned reaches the same lambda at every call, so the default-on
        // `direct-call` lever lowers `g(x)` to a DIRECT `call $__lamw{i}` (recovering
        // the lifted body's index at compile time) instead of a `call_indirect` through
        // the closure record's runtime code-index word. `g` captures `k`, so the env
        // still flows — the devirt is sound for capturing closures too. Asserting on the
        // emitted call SHAPE is the firing proof: a call-shape lever moves no heap, so
        // there is no `witchy stats` counter to check (opt.rs registry note).
        let src = r#"
fn main() -> Int:
    let k = 10
    let g = fn(x: Int): (x + k)
    (g(5) + g(7))
"#;
        let default = witchy_syntax::opt::OptSet::default_set();
        let (on, on_indirect) = call_shape(src, default);
        assert!(
            on.iter().any(|n| n.starts_with("__lamw")),
            "direct-call ON: the single-bound closure call devirtualizes to `call $__lamw` (got {on:?})",
        );
        assert_eq!(
            on_indirect, 0,
            "direct-call ON: no `call_indirect` remains for the sole closure call",
        );

        // Inverse guard: remove ONLY `direct-call` and the SAME program must revert to
        // an indirect call — proving the shape is this lever's doing, not incidental
        // codegen (an always-`__lamw` emitter would pass the ON case and lie here).
        let off_set = default.without(witchy_syntax::opt::Opt::DirectCall);
        let (off, off_indirect) = call_shape(src, off_set);
        assert!(
            !off.iter().any(|n| n.starts_with("__lamw")),
            "-direct-call: the closure call is NOT devirtualized (got {off:?})",
        );
        assert!(
            off_indirect >= 1,
            "-direct-call: the closure call stays `call_indirect` (indirect={off_indirect})",
        );
    }

    #[test]
    fn elides_bounds_check_in_counted_loop() {
        // (RFC-0034 L2 / BUG-008) Inside `for i in 0..list.length(xs)` over an
        // unreassigned `xs`, the compiler-managed counter satisfies `0 <= i < length(xs)`
        // by construction, so the default-on `bounds-elide` lever lowers `list.at(xs, i)`
        // to a direct UNCHECKED load — dropping the `call $list_at` helper that carries
        // the `i < 0 || i >= len` trap guard. With the lever off, every access keeps its
        // checked `$list_at` call (the de-opt reference the differential sweep compares).
        let src = r#"
fn main() -> Int:
    let xs = [3, 1, 4, 1, 5]
    var t = 0
    for i in 0..list.length(xs):
        t = (t + list.at(xs, i))
    t
"#;
        let default = witchy_syntax::opt::OptSet::default_set();
        let (on, _) = call_shape(src, default);
        assert!(
            !on.contains("list_at"),
            "bounds-elide ON: the counted-loop access is an unchecked load, no `call $list_at` (got {on:?})",
        );

        // Inverse guard: remove ONLY `bounds-elide` and the checked `$list_at` helper
        // call returns — proving the elision is this lever's doing.
        let off_set = default.without(witchy_syntax::opt::Opt::BoundsElide);
        let (off, _) = call_shape(src, off_set);
        assert!(
            off.contains("list_at"),
            "-bounds-elide: the access keeps its checked `call $list_at` guard (got {off:?})",
        );
    }

    /// Run `src` on the COMPILED backend under a specific optimization set (for value-
    /// parity checks across the `closure-elide` lever).
    fn run_str_opt(src: &str, opt: witchy_syntax::opt::OptSet) -> Vec<String> {
        witchy_syntax::opt::set_for_tests(Some(opt));
        let module = parse_module(src).expect("parse");
        let bytes = compile_module_binary(&module)
            .expect("compile")
            .expect("the binary path lowers this program");
        witchy_syntax::opt::set_for_tests(None);
        let (mut store, instance, captured) = instantiate_with_print(&bytes);
        instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .unwrap()
            .call(&mut store, ())
            .unwrap();
        captured.lock().unwrap().clone()
    }

    #[test]
    fn elides_nonescaping_closure_env() {
        // (RFC-0062 tier-1) `g` is bound by exactly one `let`, captures `k`, and is used
        // ONLY as a direct-call callee (`g(5)`, `g(7)`) — it never escapes. Under the
        // `closure-elide` lever its heap environment is ELIDED: NO `mk{n}` allocation, and
        // the call becomes a direct `call $__lamt{i}` that threads the capture `k` as a
        // leading argument (no env pointer, no per-call env load). The firing proof is the
        // emitted call SHAPE: `mk1` gone, `__lamt` present, `__lamw` (the boxed-devirt body)
        // absent.
        let src = r#"
fn main() -> Int:
    let k = 10
    let g = fn(x: Int): (x + k)
    (g(5) + g(7))
"#;
        let base = witchy_syntax::opt::OptSet::default_set();
        let on = base.with(witchy_syntax::opt::Opt::ClosureElide);
        let (on_calls, on_indirect) = call_shape(src, on);
        assert!(
            !on_calls.iter().any(|n| n.starts_with("mk")),
            "closure-elide ON: no env allocation — no `call $mk{{n}}` for the closure (got {on_calls:?})",
        );
        assert!(
            on_calls.iter().any(|n| n.starts_with("__lamt")),
            "closure-elide ON: the closure body is called directly, captures threaded (`__lamt`) (got {on_calls:?})",
        );
        assert!(
            !on_calls.iter().any(|n| n.starts_with("__lamw")),
            "closure-elide ON: no boxed env-devirt body (`__lamw`) remains (got {on_calls:?})",
        );
        assert_eq!(on_indirect, 0, "closure-elide ON: no `call_indirect` for an elided closure");

        // Inverse guard: remove ONLY `closure-elide` and the SAME program reverts to the
        // boxed closure — a `mk1` env allocation and a devirtualized `call $__lamw` — proving
        // the elision is this lever's doing (a phantom emitter would pass the ON case and lie).
        let (off_calls, _) = call_shape(src, base);
        assert!(
            off_calls.iter().any(|n| n.starts_with("mk")),
            "-closure-elide: the closure env is heap-allocated (`mk1`) (got {off_calls:?})",
        );
        assert!(
            off_calls.iter().any(|n| n.starts_with("__lamw"))
                && !off_calls.iter().any(|n| n.starts_with("__lamt")),
            "-closure-elide: the closure stays boxed (`__lamw`, no `__lamt`) (got {off_calls:?})",
        );
    }

    #[test]
    fn keeps_env_for_escaping_closure() {
        // (RFC-0062 default-deny) `g` is passed WHOLE into `apply_it` — it escapes the
        // frame, so even under `closure-elide` its environment MUST stay heap-allocated
        // (`mk1`) and no `__lamt` threaded body is emitted. This is the firing proof's
        // negative half: the lever fires ONLY when the escape oracle proves confinement.
        let src = r#"
fn apply_it(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main() -> Int:
    let k = 10
    let g = fn(x: Int): (x + k)
    apply_it(g, 5)
"#;
        let on = witchy_syntax::opt::OptSet::default_set().with(witchy_syntax::opt::Opt::ClosureElide);
        let (calls, _) = call_shape(src, on);
        assert!(
            calls.iter().any(|n| n.starts_with("mk")),
            "closure-elide ON but ESCAPING: the env is still heap-allocated (`mk1`) (got {calls:?})",
        );
        assert!(
            !calls.iter().any(|n| n.starts_with("__lamt")),
            "closure-elide ON but ESCAPING: no threaded body — the closure stays boxed (got {calls:?})",
        );
    }

    #[test]
    fn elided_closure_matches_boxed_output() {
        // (RFC-0062 parity) The allocation strategy is unobservable: an elided closure and
        // a boxed one must produce identical output. Covers a capture that is read AND a
        // closure invoked many times in a loop (the hot-path shape the lever targets).
        let src = r#"
fn main(console: Console):
    let base = 100
    let f = fn(x: Int): (x + base)
    var total = 0
    var i = 0
    while (i < 5):
        total = (total + f(i))
        i = (i + 1)
    print(console, "${total}")
"#;
        let base = witchy_syntax::opt::OptSet::default_set();
        let on = run_str_opt(src, base.with(witchy_syntax::opt::Opt::ClosureElide));
        let off = run_str_opt(src, base);
        // 100+0 + 101+... => (100*5) + (0+1+2+3+4) = 500 + 10 = 510.
        assert_eq!(on, vec!["510".to_string()], "elided closure computes the right value");
        assert_eq!(on, off, "elided and boxed closures produce identical output (parity)");
    }
