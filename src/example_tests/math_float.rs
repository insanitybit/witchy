use super::*;
use crate::{ast, codegen, interpreter, typeck};

    /// (BUG-326) Whole-valued Float rendering keeps a Float marker without using
    /// Rust's exact fixed-point expansion for large magnitudes. This shared
    /// formatter feeds interpolation, `show`, JSON reflection, the interpreter,
    /// and compiled wasm.
    #[test]
    fn whole_float_rendering_uses_shortest_round_trip_on_both_backends() {
        let src = "import show\nimport json\nimport reflect\n\ntype Reading derive(Reflect):\n    value: Float\n\nfn main(console: Console):\n    let big = 1234567890123456789.0\n    console.print(\"${big}\")\n    console.print(show.render(big))\n    console.print(json.stringify(Reading(big)))\n";
        let expected = [
            "1.2345678901234568e18",
            "1.2345678901234568e18",
            "{\"value\":1.2345678901234568e18}",
        ];
        assert_eq!(link_run(src), expected, "interp: whole Float rendering");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: whole Float rendering",
        );
    }

    /// (BUG-240, parity) `math.abs(Int.MIN)` has no positive `Int`, so both backends
    /// must ABORT rather than silently wrap back to the negative `Int.MIN`. Ordinary
    /// magnitudes still agree. (Was a stable wrong answer: `-Int.MIN == Int.MIN`.)
    #[test]
    fn math_abs_int_min_aborts_on_both_backends() {
        let compile = |src: &str| -> (ast::Module, Vec<u8>) {
            let linked = resolve_std_src(src);
            typeck::check(&linked).expect("typecheck");
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("the binary path lowers this program");
            (linked, bytes)
        };
        // Int.MIN: `0 - 9223372036854775807 - 1`. Both backends must error.
        let min_src = "import math\n\nfn main(console: Console):\n    console.print(\"${math.abs(0 - 9223372036854775807 - 1)}\")\n";
        let (lmod, wasm) = compile(min_src);
        assert!(
            interpreter::run_module(lmod, ".", Vec::new()).is_err(),
            "interpreter must abort on math.abs(Int.MIN)"
        );
        assert!(crate::run_wasm_bytes(&wasm).is_err(), "WASM must abort on math.abs(Int.MIN)");
        // Ordinary magnitudes agree (negative, zero, positive, and Int.MAX).
        let ok_src = "import math\n\nfn main(console: Console):\n    console.print(\"${math.abs(0 - 5)}\")\n    console.print(\"${math.abs(0)}\")\n    console.print(\"${math.abs(7)}\")\n    console.print(\"${math.abs(9223372036854775807)}\")\n";
        let expected = ["5", "0", "7", "9223372036854775807"];
        assert_eq!(link_run(ok_src), expected, "interp math.abs of ordinary values");
        assert_eq!(
            run_linked_on_wasm(&[("main", ok_src)], "main"),
            expected,
            "compiled math.abs of ordinary values must agree",
        );
    }

    /// (BUG-466, RFC-0044) `math.to_int(NaN)` is a loud contract error on both
    /// backends. Finite values and infinities keep the existing saturating
    /// truncation behavior.
    #[test]
    fn math_to_int_nan_aborts_on_both_backends() {
        let compile = |src: &str| -> (ast::Module, Vec<u8>) {
            let linked = resolve_std_src(src);
            typeck::check(&linked).expect("typecheck");
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("the binary path lowers this program");
            (linked, bytes)
        };
        let nan_src = "import math\n\nfn main(console: Console):\n    console.print(\"${math.to_int(0.0 / 0.0)}\")\n";
        let (lmod, wasm) = compile(nan_src);
        let interp_err = interpreter::run_module(lmod, ".", Vec::new())
            .expect_err("interpreter must abort on math.to_int(NaN)")
            .to_string();
        assert!(interp_err.contains("math.to_int: NaN cannot be converted to Int"), "{interp_err}");
        let wasm_err = crate::run_wasm_bytes(&wasm)
            .expect_err("WASM must abort on math.to_int(NaN)")
            .to_string();
        assert!(wasm_err.contains("math.to_int: NaN cannot be converted to Int"), "{wasm_err}");

        let ok_src = "import math\n\nfn main(console: Console):\n    console.print(\"${math.to_int(3.9)}\")\n    console.print(\"${math.to_int(0.0 - 3.9)}\")\n    console.print(\"${math.to_int(1.0 / 0.0)}\")\n    console.print(\"${math.to_int(0.0 - (1.0 / 0.0))}\")\n";
        let expected = ["3", "-3", "9223372036854775807", "-9223372036854775808"];
        assert_eq!(link_run(ok_src), expected, "interp math.to_int non-NaN cases");
        assert_eq!(
            run_linked_on_wasm(&[("main", ok_src)], "main"),
            expected,
            "compiled math.to_int non-NaN cases",
        );
    }

    /// (RFC-0052) A Float SCRUTINEE bound to a variable pattern now compiles (the
    /// former check-passes/codegen-fails hole) and agrees on both backends.
    #[test]
    fn float_scrutinee_binding_backends_agree() {
        let src = "fn main(console: Console):\n    let r = match 1.5:\n        x -> x + 1.0\n    console.print(\"${r}\")\n";
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["2.5"]);
    }

    #[test]
    fn math_isqrt_and_perfect_square_backends_agree() {
        // isqrt floors the square root (overflow-safe); is_perfect_square is
        // true exactly on 0,1,4,9,... and false for negatives. A negative isqrt
        // argument is a rule-3 abort (RFC-0044), covered by
        // std_contract_violations_abort_on_both_backends; is_perfect_square
        // short-circuits negatives to false without calling isqrt.
        let client = r#"
import math
import list
fn main(console: Console):
    let roots = list.map([0, 1, 2, 3, 4, 8, 9, 15, 16, 100, 99], fn(n: Int): math.isqrt(n))
    console.print(list.join(list.map(roots, fn(n: Int): "${n}"), ","))
    let flags = list.map([0, 1, 2, 4, 9, 10, 16, 17], fn(n: Int): if math.is_perfect_square(n): "T" else: "F")
    console.print(list.join(flags, ""))
    console.print(if math.is_perfect_square(-4): "T" else: "F")
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
        assert_eq!(compiled, vec!["0,1,1,1,2,2,3,3,4,10,9", "TTFTTFTF", "F"]);
    }

    #[test]
    fn math_ceil_and_round_div_backends_agree() {
        // ceil_div rounds toward +inf for the quotient; round_div rounds to the
        // nearest integer (ties away from zero). Both for a positive divisor.
        let client = r#"
import math
import list
fn show(xs: List(Int)) -> String:
    list.join(list.map(xs, fn(n: Int): "${n}"), ",")
fn main(console: Console):
    console.print(show([math.ceil_div(7, 3), math.ceil_div(6, 3), math.ceil_div(1, 3), math.ceil_div(0, 3)]))
    console.print(show([math.ceil_div(0 - 7, 3), math.ceil_div(0 - 6, 3)]))
    console.print(show([math.round_div(7, 2), math.round_div(5, 3), math.round_div(4, 3), math.round_div(0 - 7, 2)]))
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
        // zero is "0", negatives get a "-". An out-of-range base fails loudly
        // (RFC-0044 rule 3) — covered in std_contract_violations_abort_on_both_backends.
        let client = r#"
import math
fn main(console: Console):
    console.print(math.to_hex(255))
    console.print(math.to_hex(0))
    console.print(math.to_hex(4096))
    console.print(math.to_binary(5))
    console.print(math.to_base(255, 16))
    console.print(math.to_base(0 - 255, 16))
    console.print(math.to_base(0, 2))
"#;
        let sources = [("math", crate::bundled_module("math").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "to_base diverged");
        assert_eq!(
            compiled,
            vec!["ff", "0", "1000", "101", "ff", "-ff", "0"]
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
    console.print(math.format_float(3.14159, 2))
    console.print(math.format_float(0.0 - 0.5, 1))
    console.print(math.format_float(2.0, 0))
    console.print(math.format_float(0.0, 2))
    console.print(math.format_float(1.999, 2))
    console.print(math.format_float(0.0 - 0.04, 1))
    console.print(math.format_float(98.6, 1))
"#;
        let sources = [("math", crate::bundled_module("math").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "format_float diverged");
        assert_eq!(compiled, vec!["3.14", "-0.5", "2", "0.00", "2.00", "0.0", "98.6"]);
    }

    #[test]
    fn floats_in_collections_backends_agree() {
        // 8-byte slots also hold f64, so floats now live in lists and tuples
        // (read back with float_to_int, since Float to_string is still WASM-gated).
        let client = r#"
fn main(console: Console):
    let fs = [1.5, 2.5, 3.5]
    console.print("${list.length(fs)}")
    console.print("${math.to_int(list.at(fs, 1))}")
    let pair = (1.5, 9.5)
    let (lo, hi) = pair
    console.print("${math.to_int(hi)}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "floats-in-collections diverged");
        assert_eq!(compiled, vec!["3", "2", "9"]);
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
    ((d - math.sqrt(4.0)) + math.float_min(2.5, math.sqrt(2.25)) + math.float_clamp(5.0, 0.0, 1.0))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "compiled float arithmetic diverged");
        assert_eq!(compiled, vec!["5.5"]);
    }

    #[test]
    fn std_math_factorial_is_prime_backends_agree() {
        let client = r#"
import math

fn main(console: Console):
    console.print("${math.factorial(5)}")
    console.print("${math.factorial(0)}")
    console.print("${math.factorial(1)}")
    console.print("${math.is_prime(7)}")
    console.print("${math.is_prime(12)}")
    console.print("${math.is_prime(1)}")
    console.print("${math.is_prime(2)}")
    console.print("${math.is_prime(97)}")
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
    console.print("${math.lcm(4, 6)}")
    console.print("${math.lcm(21, 6)}")
    console.print("${math.lcm(0, 5)}")
    console.print("${math.lcm((0 - 4), 6)}")
    console.print("${math.is_even(10)}")
    console.print("${math.is_odd(7)}")
    console.print("${math.is_odd((0 - 3))}")
"#;
        let sources = [("math", crate::bundled_module("math").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "math lcm/parity diverged");
        assert_eq!(compiled, vec!["12", "42", "0", "12", "true", "true", "true"]);
    }
