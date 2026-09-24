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

