use super::*;
use crate::codegen;

    /// (BUG-481) Numeric duration constructors are convenience contracts, not
    /// wrapping arithmetic. Oversized counts abort before the intermediate Int
    /// multiplication/addition can wrap; ordinary negative spans remain valid.
    #[test]
    fn duration_numeric_constructors_abort_on_overflow_on_both_backends() {
        let ok = "import duration\n\nfn main(console: Console):\n    console.print(duration.human(duration.seconds(0 - 90)))\n    console.print(duration.human(duration.from_clock(1, 2, 3)))\n";
        let expected = ["-1m30s", "1h2m3s"];
        assert_eq!(
            run_linked_on_wasm(&[("main", ok)], "main"),
            expected,
            "compiled: duration constructor controls",
        );

        for (label, call) in [
            ("seconds", "duration.seconds(9223372036854776)"),
            ("days", "duration.days(200000000000)"),
            ("from_clock", "duration.from_clock(2562047788015, 13, 0)"),
        ] {
            let src = format!("import duration\n\nfn main(console: Console):\n    console.print(\"${{{call}}}\")\n");
            let linked = resolve_std_src(&src);
            let wasm = codegen::compile_module_binary(&linked)
                .expect_lowered("duration overflow program should lower");
            let wasm_err = crate::run_wasm_bytes(&wasm)
                .expect_err("WASM must abort on duration overflow")
                .to_string();
            assert!(
                wasm_err.contains("duration.") && wasm_err.contains("overflow"),
                "{label}: {wasm_err}"
            );
        }
    }

    #[test]
    fn duration_literals_backends_agree() {
        // Native duration literals (1s/1ms/1m/1h/1d/1w, and the `hr` alias) are a
        // distinct Duration type carried as milliseconds: they add/subtract,
        // scale by an Int, divide to an Int ratio, and compare — identically on
        // both backends.
        let client = r#"
fn main(console: Console):
    console.print("${30s > 500ms}")
    console.print("${30s + 500ms == 30500ms}")
    console.print("${1m == 60s}")
    console.print("${2hr == 7200s}")
    console.print("${1d == 24h}")
    console.print("${1w > 6d}")
    console.print("${2 * 1h == 7200s}")
    console.print("${1h / 1m == 60}")
"#;
        let sources = [("main", client)];
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(
            compiled,
            vec!["true", "true", "true", "true", "true", "true", "true", "true"]
        );
    }

    #[test]
    fn prng_module_backends_agree() {
        // The Park-Miller LCG replays a deterministic sequence (the canonical
        // seed-1 values) identically on both backends; next_below bounds it.
        let client = r#"
import prng
import list
fn main(console: Console):
    var r = prng.seed(1)
    var out = []
    var i = 0
    while i < 4:
        let n = prng.next(r)
        list.push(out, n)
        i = i + 1
    console.print(list.join(list.map(out, fn(n: Int): "${n}"), ","))
    var r3 = prng.seed(42)
    let d = prng.next_below(r3, 6)
    console.print("${d}")
    var r4 = prng.seed(2)
    let b = prng.next_bool(r4)
    console.print(if b: "even" else: "odd")
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("prng", crate::bundled_module("prng").unwrap()),
            ("main", client),
        ];
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(
            compiled,
            vec!["16807,282475249,1622650073,984943658", "0", "even"]
        );
    }
