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

