use super::*;
use crate::interpreter;

    /// A large `Int` carried through an *unbounded* generic function must keep its
    /// 64 bits on WASM. The generic i32 ABI truncated it; the WASM backend now
    /// monomorphizes the call on `Int` (`fill__Int`), so the i64 survives.
    /// (Regression for the big-int-through-generic gap.)
    #[test]
    fn wasm_monomorphizes_big_int_through_generic() {
        let src = "fn fill(x: a, n: Int) -> List(a):\n    var out = []\n    var i = 0\n    while i < n:\n        list.push(out, x)\n        i = i + 1\n    out\n\nfn main(console: Console):\n    let xs = fill(5000000000, 2)\n    console.print(\"${list.at(xs, 0)}\")\n";
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
        let src = "fn apply(f: fn(Int) -> Int, x: Int) -> Int:\n    f(x)\n\nfn main(console: Console):\n    console.print(\"${apply(fn(k: Int): k * 5000000000, 2)}\")\n";
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
        let arg = "fn apply(f: fn(Int) -> Int, x: Int) -> Int:\n    f(x)\n\nfn main(console: Console):\n    console.print(\"${apply(fn(k: Int): k + 1, 5000000000)}\")\n";
        assert_eq!(interp(arg), vec!["5000000001"], "interpreter (arg)");
        assert_eq!(run_on_wasm(arg), vec!["5000000001"], "WASM (arg)");
        // Capture: a big Int captured by the closure, recovered from the env.
        let cap = "fn apply(f: fn(Int) -> Int, x: Int) -> Int:\n    f(x)\n\nfn main(console: Console):\n    let big = 5000000000\n    console.print(\"${apply(fn(x: Int): x + big, 1)}\")\n";
        assert_eq!(interp(cap), vec!["5000000001"], "interpreter (capture)");
        assert_eq!(run_on_wasm(cap), vec!["5000000001"], "WASM (capture)");
    }

    /// A closure RETURNED from a function and bound to a `let` (currying) must
    /// keep a big `Int` result on WASM: the binding records the closure's
    /// call-return kind (from the `-> fn(...) -> RET` declaration), so the later
    /// `f(x)` recovers at i64. (Regression for the let-bound-closure-return gap.)
    #[test]
    fn wasm_big_int_through_curried_closure() {
        let src = "fn make(big: Int) -> fn(Int) -> Int:\n    fn(x: Int): x + big\n\nfn main(console: Console):\n    let f = make(5000000000)\n    console.print(\"${f(1)}\")\n";
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
        let src = "fn pair(x: a, y: a) -> (a, a):\n    (x, y)\n\nfn main(console: Console):\n    let (p, q) = pair(9000000000, 1)\n    console.print(\"${p}\")\n    console.print(\"${q}\")\n";
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
        let big = "fn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, \"k\", 9000000000)\n    console.print(\"${dict.get_or(d, \"k\", 0)}\")\n";
        assert_eq!(interp(big), vec!["9000000000"], "interpreter");
        assert_eq!(run_on_wasm(big), vec!["9000000000"], "WASM");
        let s = "fn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, \"a\", \"hello\")\n    console.print(dict.get_or(d, \"a\", \"none\"))\n";
        assert_eq!(interp(s), vec!["hello"], "interpreter (string value)");
        assert_eq!(run_on_wasm(s), vec!["hello"], "WASM (string value)");
    }

    /// Iterating a `Dict`'s `dict.values()` (or binding the list) must keep big-Int
    /// values 64-bit: codegen tracks the Dict's value type from `insert` and
    /// carries it to `dict.values(d)`, so the loop variable is i64.
    #[test]
    fn wasm_dict_values_iteration_keeps_big_ints() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, \"k\", 9000000000)\n    var s = 0\n    for v in dict.values(d):\n        s = s + v\n    console.print(\"${s}\")\n";
        assert_eq!(interp(src), vec!["9000000000"], "interpreter");
        assert_eq!(run_on_wasm(src), vec!["9000000000"], "WASM");
    }

    /// A big `Int` in a tuple ELEMENT of a list must survive being read back —
    /// `list.at(list_of_tuples, i)` then destructured, and `for t in list_of_tuples`.
    /// Codegen tracks a list's element-tuple slot types (literal or variable) and
    /// applies them to the `at`/loop tuple destructure. (Two-level nesting.)
    #[test]
    fn wasm_big_int_in_list_of_tuples() {
        let direct = "fn main(console: Console):\n    let (a, b) = list.at([(9000000000, 1)], 0)\n    console.print(\"${a}\")\n    console.print(\"${b}\")\n";
        assert_eq!(interp(direct), vec!["9000000000", "1"], "interpreter (direct)");
        assert_eq!(run_on_wasm(direct), vec!["9000000000", "1"], "WASM (direct)");
        let loop_src = "fn main(console: Console):\n    for t in [(9000000000, 1)]:\n        let (a, b) = t\n        console.print(\"${a}\")\n";
        assert_eq!(interp(loop_src), vec!["9000000000"], "interpreter (loop)");
        assert_eq!(run_on_wasm(loop_src), vec!["9000000000"], "WASM (loop)");
    }

    /// A big `Int` in a nested list (`list.at(list.at(xs, i), j)`) must survive. Codegen
    /// tracks a list-of-lists' inner element type so the inner `at` recovers it
    /// as i64. (Two levels of list nesting — e.g. a matrix row/column.)
    #[test]
    fn wasm_big_int_in_nested_list() {
        let src = "fn main(console: Console):\n    let m = [[1, 9000000000], [3, 4]]\n    console.print(\"${list.at(list.at(m, 0), 1)}\")\n";
        assert_eq!(interp(src), vec!["9000000000"], "interpreter");
        assert_eq!(run_on_wasm(src), vec!["9000000000"], "WASM");
    }

    /// A generic function over `List((a, b))` (the `zip`/`unzip` shape) must keep
    /// big Ints. Monomorphization resolves `a`/`b` from the argument list's
    /// element tuple, the inner `let (x, y) = p` destructures at i64, and the
    /// `List(a)` return carries the element type. (The deepest nesting case.)
    #[test]
    fn wasm_big_int_through_list_of_tuples_generic() {
        let src = "fn firsts(ps: List((a, b))) -> List(a):\n    var out = []\n    for p in ps:\n        let (x, y) = p\n        list.push(out, x)\n    out\n\nfn main(console: Console):\n    console.print(\"${list.at(firsts([(9000000000, 1)]), 0)}\")\n";
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
        let chain = "fn main(console: Console):\n    let xs = [[[[9000000000]]]]\n    console.print(\"${list.at(list.at(list.at(list.at(xs, 0), 0), 0), 0)}\")\n";
        assert_eq!(interp(chain), vec!["9000000000"], "interpreter (at-chain)");
        assert_eq!(run_on_wasm(chain), vec!["9000000000"], "WASM (at-chain)");
        // Depth-3 nested loops through a nested-list parameter.
        let loops = "fn total(c: List(List(List(Int)))) -> Int:\n    var s = 0\n    for plane in c:\n        for row in plane:\n            for x in row:\n                s = s + x\n    s\n\nfn main(console: Console):\n    console.print(\"${total([[[9000000000]]])}\")\n";
        assert_eq!(interp(loops), vec!["9000000000"], "interpreter (loops/param)");
        assert_eq!(run_on_wasm(loops), vec!["9000000000"], "WASM (loops/param)");
    }

    /// A big `Int` in a tuple at the bottom of NESTED lists (`[[(big, 1)]]`)
    /// survives: the `(depth, bottom)` nesting allows a tuple bottom, so peeling
    /// to the inner list then destructuring the tuple recovers the Int as i64.
    #[test]
    fn wasm_big_int_in_nested_list_of_tuples() {
        let src = "fn main(console: Console):\n    for inner in [[(9000000000, 1)]]:\n        for t in inner:\n            let (a, b) = t\n            console.print(\"${a}\")\n";
        assert_eq!(interp(src), vec!["9000000000"], "interpreter");
        assert_eq!(run_on_wasm(src), vec!["9000000000"], "WASM");
    }

    /// A large `Int` carried as an `Option`/`Result` success payload must keep its
    /// 64 bits on WASM, through both `?` and a `match`. The payload field is a type
    /// variable (generic i32 ABI), so codegen would truncate; it now tracks the
    /// declared scalar payload type and recovers `Some`/`Ok` values (and `?`
    /// results) at i64. (Regression for the big-Int-through-Option/Result gap.)
    #[test]
    fn wasm_big_int_through_result_payload_and_try() {
        let src = "fn fetch() -> Result(Int, String):\n    Ok(5000000000)\n\nfn chain() -> Result(Int, String):\n    let x = (fetch())?\n    Ok((x + 1))\n\nfn main(console: Console):\n    match chain():\n        Ok(v) -> console.print(\"${v}\")\n        Err(e) -> console.print(e)\n";
        let want = vec!["5000000001".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    #[test]
    fn big_int_arithmetic_backends_agree() {
        // Compiled Int is now i64, so arithmetic beyond the old 32-bit range
        // agrees with the interpreter instead of wrapping.
        let client = r#"
fn main(console: Console):
    let a = 3000000000
    let b = 4000000000
    console.print("${a + b}")
    console.print("${a * 3}")
    let big = 9000000000000
    console.print("${big}")
    console.print("${big / 1000}")
    console.print("${0 - big}")
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
    console.print("${list.at(xs, 0)}")
    console.print("${list.at(xs, 1)}")
    console.print("${list.at(xs, 0) + list.at(xs, 1)}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "big-ints-in-list diverged");
        assert_eq!(compiled, vec!["3000000000", "5000000000", "8000000000"]);
    }
