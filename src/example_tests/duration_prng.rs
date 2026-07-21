use super::*;
use crate::{codegen, interpreter};

    /// (BUG-481) Numeric duration constructors are convenience contracts, not
    /// wrapping arithmetic. Oversized counts abort before the intermediate Int
    /// multiplication/addition can wrap; ordinary negative spans remain valid.
    #[test]
    fn duration_numeric_constructors_abort_on_overflow_on_both_backends() {
        let ok = "import duration\n\nfn main(console: Console):\n    console.print(duration.human(duration.seconds(0 - 90)))\n    console.print(duration.human(duration.from_clock(1, 2, 3)))\n";
        let expected = ["-1m30s", "1h2m3s"];
        assert_eq!(link_run(ok), expected, "interp: duration constructor controls");
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
            let interp_err = interpreter::run_module(linked.clone(), ".", Vec::new())
                .expect_err("interpreter must abort on duration overflow")
                .to_string();
            assert!(
                interp_err.contains("duration.") && interp_err.contains("overflow"),
                "{label}: {interp_err}"
            );
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

    /// `rand.below` fails loudly for an impossible range (RFC-0044 rule 3,
    /// matching `prng.next_below`) — and still draws for a valid bound.
    #[test]
    fn rand_below_rejects_nonpositive_bound_on_both_backends() {
        let bad = "import rand\nfn main(console: Console, r: Rand):\n    console.print(\"${rand.below(r, 0)}\")\n";
        let want_core = "rand.below: bound `0` must be positive";
        let linked = resolve_std_src(bad);
        let ierr = interpreter::run_module(linked.clone(), ".", Vec::new())
            .expect_err("interpreter must abort");
        assert!(
            ierr.message.ends_with(want_core),
            "interpreter core mismatch: got `{}`, want suffix `{want_core}`",
            ierr.message
        );
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let cerr = crate::run_wasm_bytes(&bytes).expect_err("WASM must abort");
        assert_eq!(cerr, format!("runtime error: {}", ierr.message), "compiled abort mismatch");

        // A valid bound still draws a value in range on both backends.
        let ok = "import rand\nfn main(console: Console, r: Rand):\n    let n = rand.below(r, 10)\n    console.print(\"${n >= 0 && n < 10}\")\n";
        assert_eq!(link_run(ok), vec!["true"], "interpreter valid bound");
        assert_eq!(wasm_run(ok), vec!["true"], "compiled valid bound");
    }

    #[test]
    fn prng_next_below_rejects_uncoverable_bound_backends_agree() {
        // The Park-Miller reducer is `n % bound`; a bound at or above the generator
        // range (2^31-1) cannot cover its own range, so it fails loudly (BUG-482)
        // — like the non-positive guard. An ordinary small bound still draws.
        let bad = r#"
import prng
fn main(console: Console):
    var r = prng.seed(1)
    let _i = prng.next_below(r, 2147483647)
    console.print("unreachable")
"#;
        let linked = resolve_std_src(bad);
        let ierr =
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect_err("interpreter must abort");
        assert!(
            ierr.message.contains("cannot be covered"),
            "interpreter core mismatch: {}",
            ierr.message
        );
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let cerr = crate::run_wasm_bytes(&bytes).expect_err("WASM must abort");
        assert!(cerr.contains("cannot be covered"), "compiled core mismatch: {cerr}");

        let ok = "import prng\nfn main(console: Console):\n    var r = prng.seed(1)\n    let i = prng.next_below(r, 6)\n    console.print(\"${i >= 0 && i < 6}\")\n";
        assert_eq!(link_run(ok), vec!["true"], "interpreter small bound");
        assert_eq!(wasm_run(ok), vec!["true"], "compiled small bound");
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
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "duration literals diverged");
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
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "prng diverged");
        assert_eq!(
            compiled,
            vec!["16807,282475249,1622650073,984943658", "0", "even"]
        );
    }

    #[test]
    fn prng_choice_backends_agree() {
        // choice picks a pseudo-random element (None for an empty list),
        // deterministically for a given seed, identically on both backends.
        let client = r#"
import prng
import option
fn main(console: Console):
    var r = prng.seed(1)
    let c = prng.choice(["a", "b", "c", "d"], r)
    console.print(option.unwrap_or(c, "?"))
    var r2 = prng.seed(1)
    let e = prng.choice([], r2)
    console.print(option.unwrap_or(e, "empty"))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("prng", crate::bundled_module("prng").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "prng.choice diverged");
        assert_eq!(compiled, vec!["d", "empty"]);
    }

    #[test]
    fn duration_module_backends_agree() {
        // The duration module over the built-in Duration type: human/clock format
        // a Duration (combined from literals), to_milliseconds bridges back to Int,
        // and the whole-unit total conversions (to_seconds..to_weeks) truncate.
        let client = r#"
import duration
fn main(console: Console):
    console.print("${duration.to_milliseconds(duration.from_clock(1, 2, 3))}")
    console.print(duration.clock(1h + 2m + 3s))
    console.print(duration.clock(90s))
    console.print(duration.human(1h + 1m + 1s))
    console.print(duration.human(90s))
    console.print(duration.human(5s))
    console.print(duration.human(500ms))
    console.print("${duration.to_milliseconds(duration.hours(2))}")
    console.print("${duration.part_minutes(1h + 2m + 3s)}")
    console.print("${duration.to_seconds(duration.days(10))}")
    console.print("${duration.to_minutes(duration.days(10))}")
    console.print("${duration.to_hours(duration.days(10))}")
    console.print("${duration.to_days(duration.days(10))}")
    console.print("${duration.to_weeks(duration.days(10))}")
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
                "864000", "14400", "240", "10", "1",
            ]
        );
    }

    #[test]
    fn duration_parse_backends_agree() {
        // parse is the inverse of human, returning a Duration (ms): unit-tagged
        // (incl. ms/hr) or bare-ms input, Err on junk/dangling (RFC-0044 rule 2),
        // and parse(human(d)) round-trips.
        let client = r#"
import duration
fn show(o: Result(Duration, duration.DurationParseError)) -> String:
    match o:
        Ok(d) -> "${duration.to_milliseconds(d)}"
        Err(_) -> "none"
fn roundtrip(d: Duration) -> String:
    match duration.parse(duration.human(d)):
        Ok(p) -> if p == d: "ok" else: "bad"
        Err(_) -> "none"
fn main(console: Console):
    console.print(show(duration.parse("1h2m3s")))
    console.print(show(duration.parse("500ms")))
    console.print(show(duration.parse("2hr")))
    console.print(show(duration.parse("90")))
    console.print(show(duration.parse("1h30")))
    console.print(show(duration.parse("")))
    console.print(show(duration.parse("abc")))
    console.print(roundtrip(1h + 1m + 1s))
    console.print(roundtrip(90s))
    console.print(roundtrip(250ms))
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

    /// REGRESSION (BUG-189/BUG-413): `duration.parse` returns a reachable `Err` for
    /// a unit with no preceding count (`"ms"`) and for an overflowing value (rather
    /// than `Ok(0)` or a silently-wrapped, backend-divergent number), and
    /// `duration.abs` saturates the most-negative value instead of staying negative.
    #[test]
    fn duration_parse_and_abs_edge_cases_backends_agree() {
        let src = "import duration\nfn tag(r: Result(Duration, duration.DurationParseError)) -> String:\n    match r:\n        Ok(d) -> \"ok:\" + \"${duration.to_milliseconds(d)}\"\n        Err(_e) -> \"err\"\nfn main(console: Console):\n    console.print(tag(duration.parse(\"ms\")))\n    console.print(tag(duration.parse(\"1h2m3s\")))\n    console.print(tag(duration.parse(\"99999999999999999999w\")))\n    console.print(\"${duration.to_milliseconds(duration.abs(duration.milliseconds(0 - 9223372036854775807 - 1)))}\")\n";
        let expected = ["err", "ok:3723000", "err", "9223372036854775807"];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked, ".", Vec::new()).expect("interp"),
            expected,
            "interp"
        );
        assert_eq!(wasm_run(src), expected, "wasm");
    }
