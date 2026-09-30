use super::*;
use crate::codegen;

    /// (RFC-0045) The message property: the compiled backend aborts with the
    /// exact expected message, not merely "an error". Covers each routed abort
    /// class: `fail(msg)` (dynamic), list-index OOB and `string.to_int` junk
    /// (static + dynamic data), and NaN ordering (static). A compiled trap at the
    /// wrong site or for the wrong reason would diverge here. The expected
    /// messages are independent literals (the source walker is not RFC-0167
    /// evidence), and the programs compile in parallel.
    #[test]
    fn abort_messages_match_across_backends() {
        // Each case: (program, the exact abort message, with its source site).
        let cases: &[(&str, &str)] = &[
            (
                "fn main(console: Console):\n    fail(\"the reason\")\n",
                "`main`, line 2: the reason",
            ),
            (
                "import list\nfn main(console: Console):\n    let xs = [1, 2]\n    console.print(\"${list.at(xs, 5)}\")\n",
                "`main`, line 4: list index 5 out of bounds (length 2)",
            ),
            (
                "fn main(console: Console):\n    console.print(\"${\"junk\".to_int()}\")\n",
                "`main`, line 2: cannot parse `junk` as an Int",
            ),
            (
                "fn main(console: Console):\n    let nan = 0.0 / 0.0\n    console.print(\"${nan < 1.0}\")\n",
                "`main`, line 3: cannot compare NaN",
            ),
            // (RFC-0045 / latent i32-wrap hole) A list index beyond i32 range
            // must abort with its TRUE value — `$list_at` checks in i64, so a
            // huge index can't wrap to an in-range i32 and read a bogus slot.
            // `4294967297` = 2^32 + 1 (wraps to 1 as i32) is the regression seed.
            (
                "import list\nfn main(console: Console):\n    let xs = [10, 20]\n    console.print(\"${list.at(xs, 4294967297)}\")\n",
                "`main`, line 4: list index 4294967297 out of bounds (length 2)",
            ),
        ];
        std::thread::scope(|scope| {
            let handles: Vec<_> = cases
                .iter()
                .map(|(src, want)| {
                    scope.spawn(move || {
                        let linked = prepared_module(src);
                        let bytes = codegen::compile_module_binary(&linked)
                            .expect_lowered("the binary path lowers this program");
                        let cerr = crate::run_wasm_bytes(&bytes).expect_err("WASM must abort");
                        assert_eq!(cerr, format!("runtime error: {want}"), "compiled abort mismatch for src:\n{src}");
                    })
                })
                .collect();
            for handle in handles {
                handle.join().expect("abort case");
            }
        });
    }

    /// (RFC-0044 rule 3) The Witchy std contract-violation path aborts: a bad
    /// argument that used to silently default now aborts on admitted Wasm.
    /// Each case pairs a program with its independently specified message core.
    #[test]
    fn std_contract_violations_abort_with_expected_messages() {
        if !is_nightly() {
            return;
        }
        let cases: &[(&str, &str)] = &[
            (
                "import math\nfn main(console: Console):\n    console.print(\"${math.factorial(-5)}\")\n",
                "math.factorial: `-5` is negative (expected n >= 0)",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(\"${math.pow(2, -1)}\")\n",
                "math.pow: exponent `-1` is negative (expected exp >= 0)",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(\"${math.isqrt(-5)}\")\n",
                "math.isqrt: `-5` is negative (expected n >= 0)",
            ),
            (
                "import time\nfn main(console: Console):\n    console.print(\"${time.days_in_month(2026, 13)}\")\n",
                "time.days_in_month: month `13` is out of range (expected 1..12)",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(math.to_base(10, 17))\n",
                "math.to_base: base `17` is outside 2..16",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(math.to_base(10, 1))\n",
                "math.to_base: base `1` is outside 2..16",
            ),
            (
                "fn main(console: Console):\n    console.print(\"x\".pad_left(3, \"\"))\n",
                "string.pad_left: empty `fill` cannot pad to width 3",
            ),
            (
                "fn main(console: Console):\n    console.print(\"x\".pad_right(3, \"\"))\n",
                "string.pad_right: empty `fill` cannot pad to width 3",
            ),
            (
                "fn main(console: Console):\n    console.print(\"x\".center(3, \"\"))\n",
                "string.center: empty `fill` cannot pad to width 3",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(\"${math.clamp(5, 10, 0)}\")\n",
                "math.clamp: lo `10` exceeds hi `0`",
            ),
            (
                "import cmp\nfn main(console: Console):\n    console.print(\"${cmp.clamp(5, 10, 0)}\")\n",
                "cmp.clamp: lo exceeds hi (an empty range)",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(\"${math.ceil_div(7, 0)}\")\n",
                "math.ceil_div: divisor `0` must be positive",
            ),
            (
                "import math\nfn main(console: Console):\n    console.print(\"${math.round_div(7, -2)}\")\n",
                "math.round_div: divisor `-2` must be positive",
            ),
            (
                "import semver\nfn main(console: Console):\n    console.print(semver.format(semver.version(-1, 2, 3)))\n",
                "semver.version: components `-1.2.3` must be non-negative",
            ),
        ];
        for (src, want_core) in cases {
            let linked = prepared_module(src);
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("the binary path lowers this program");
            let cerr = crate::run_wasm_bytes(&bytes).expect_err("WASM must abort");
            assert!(cerr.starts_with("runtime error: ") && cerr.ends_with(want_core),
                "compiled abort mismatch for src:\n{src}\ngot: {cerr}\nwant suffix: {want_core}");
        }
        // The valid-boundary values still work (no over-eager abort): factorial(0),
        // pow(x, 0), isqrt(0), days_in_month for every month 1..12, to_base at both
        // ends of 2..16, and an empty fill when the string is already wide enough
        // (no padding is needed, so nothing is violated).
        let ok = "import math\nimport time\nfn main(console: Console):\n    console.print(\"${math.factorial(0)}\")\n    console.print(\"${math.pow(2, 0)}\")\n    console.print(\"${math.isqrt(0)}\")\n    console.print(\"${time.days_in_month(2024, 2)}\")\n    console.print(\"${time.days_in_month(2026, 2)}\")\n    console.print(\"${time.days_in_month(2026, 12)}\")\n    console.print(math.to_base(10, 2))\n    console.print(math.to_base(255, 16))\n    console.print(\"abc\".pad_left(3, \"\"))\n    console.print(\"abcd\".center(3, \"\"))\n";
        let want = vec!["1", "1", "0", "29", "28", "31", "1010", "ff", "abc", "abcd"];
        assert_eq!(wasm_run(ok), want, "compiled boundary");
    }
