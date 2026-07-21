use super::*;
use crate::{codegen, interpreter, parser, typeck};

    /// (BUG-276) The public hex decoders reject malformed input before it can
    /// reach the private raw byte-level primitives (`encoding.hex_decode_lossy`,
    /// `encoding.hex_to_base64url_lossy`). Invalid input is `Err` on both
    /// backends, never the old silent-drop that could hand mangled crypto
    /// material to a signature check. Valid hex still round-trips.
    #[test]
    fn hex_primitives_reject_non_hex_strictly_on_both_backends() {
        let prog = |call: &str| {
            format!(
                "import encoding\n\nfn main(console: Console):\n    match {call}:\n        Ok(x) -> console.print(x)\n        Err(e) -> console.print(\"err\")\n"
            )
        };
        for bad in [
            "encoding.hex_decode(\"68zz69\")",
            "encoding.hex_to_base64url(\"zz6869\")",
            "encoding.hex_decode(\"abc\")", // odd length
        ] {
            let src = prog(bad);
            assert_eq!(link_run(&src), ["err"], "interpreter must reject non-hex: {bad}");
            assert_eq!(
                run_linked_on_wasm(&[("main", &src)], "main"),
                ["err"],
                "WASM must reject non-hex: {bad}"
            );
        }
        // Valid hex still decodes identically on both backends.
        let ok = prog("encoding.hex_decode(\"6869\")");
        assert_eq!(link_run(&ok), ["hi"], "interp decodes valid hex");
        assert_eq!(run_linked_on_wasm(&[("main", &ok)], "main"), ["hi"], "wasm decodes valid hex");
    }

    /// Python-style f-strings: `f"...{expr}..."` interpolates (with `{{`/`}}` for
    /// literal braces), desugaring to generated render + concat — same result on
    /// both backends.
    #[test]
    fn f_strings_interpolate() {
        let src = "fn main(console: Console):\n    let name = \"world\"\n    let n = 6\n    console.print(f\"hi {name} #{n * 7}\")\n    console.print(f\"{{braces}}\")\n";
        assert_eq!(interp(src), vec!["hi world #42", "{braces}"]);
        assert_eq!(run_on_wasm(src), vec!["hi world #42", "{braces}"]);
    }

    /// THE F11 FAMILY (learning log): interpolating values whose type only
    /// typed lowering knows — an ADT String payload and a generic-combinator
    /// return — renders identically on both backends.
    #[test]
    fn interpolation_of_mono_typed_values_agrees() {
        let src = "import iter\n\ntype Msg:\n    Text(String)\n    Silence\n\nfn main(console: Console):\n    match Text(\"hi\"):\n        Text(s) -> console.print(\"got: ${s}\")\n        Silence -> console.print(\"none\")\n    let collected: List(Int) = iter.collect(iter.range(1, 100).take(3))\n    console.print(\"collected: ${collected}\")\n";
        let want: Vec<String> = ["got: hi", "collected: [1, 2, 3]"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    #[test]
    fn interpolation_tail_after_guard_returns_string() {
        let src = "fn checked(n: Int) -> String:\n    if n < 0:\n        fail(\"bad\")\n    \"${n}\"\n\nfn main(console: Console):\n    console.print(checked(7))\n";
        let want = vec!["7".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// The formatter ROUND-TRIPS string interpolation. The lexer desugars it to
    /// a generated render chain, and `interpolation_sugar` prints that AST back
    /// to the public interpolation spelling.
    #[test]
    fn fmt_round_trips_interpolation() {
        let src = "fn main(console: Console):\n    let n = 3\n    console.print(\"n is ${n}, doubled ${n * 2}\")\n    console.print(\"cost: \\$${n}\")\n";
        assert_eq!(crate::format::reformat(src).as_deref(), Some(src), "interpolation must round-trip");
    }

    /// `std/encoding` — hex + base64 over UTF-8 bytes (native, like crypto),
    /// matching the standard vectors incl. padding, and round-tripping multibyte
    /// UTF-8.
    #[test]
    fn encoding_module_hex_and_base64() {
        let src = r#"import encoding

fn main(console: Console):
    console.print(encoding.hex_encode("hello"))
    console.print(encoding.hex_decode("68656c6c6f").unwrap_or("?"))
    console.print(encoding.base64_encode("Man"))
    console.print(encoding.base64_encode("Ma"))
    console.print(encoding.base64_decode("aGVsbG8=").unwrap_or("?"))
    console.print(yn(encoding.base64_decode(encoding.base64_encode("witchy! 🧙")).unwrap_or("?") == "witchy! 🧙"))

fn yn(b: Bool) -> String:
    if b: "y" else: "n"
"#;
        assert_eq!(
            link_run(src),
            vec!["68656c6c6f", "hello", "TWFu", "TWE=", "hello", "y"]
        );
    }

    /// The `examples/time_and_encoding/src/time_and_encoding.witchy` showcase runs: a formatted civil
    /// date and base64/hex of a multibyte-UTF-8 payload, round-tripped — its
    /// footprint is just Console.
    #[test]
    fn time_and_encoding_example_runs() {
        assert_eq!(
            crate::execute_file("examples/time_and_encoding/src/time_and_encoding.witchy", Vec::new()).unwrap(),
            vec![
                "date:    2026-05-28T20:26:40Z (Thursday)",
                "layout:  Thursday, May 28 2026 at 20:26",
                "parsed:  2026-06-08T20:30:00Z",
                "checked: day 30 is out of range for 2026-2",
                "base64:  d2l0Y2h5IPCfp5k=",
                "hex:     77697463687920f09fa799",
                "decoded: witchy 🧙",
            ]
        );
        let src = std::fs::read_to_string("examples/time_and_encoding/src/time_and_encoding.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console");
    }

    /// Regression (found by `examples/calc/src/calc.witchy` via the both-backends invariant):
    /// comparing a String whose type isn't locally tracked — a List(String)
    /// element via `at` — to a literal must be a *structural* `$str_eq` on the
    /// WASM backend, not a pointer compare, with the literal on either side.
    #[test]
    fn wasm_string_eq_uses_str_eq_when_literal_on_either_side() {
        let src = "fn main(console: Console):\n    let cs = [\"a\", \" \", \"z\"]\n    console.print(if list.at(cs, 1) == \" \": \"eq\" else: \"ne\")\n    console.print(if \"a\" == list.at(cs, 0): \"eq\" else: \"ne\")\n    console.print(if list.at(cs, 0) == \"z\": \"eq\" else: \"ne\")\n";
        let want = vec!["eq".to_string(), "eq".to_string(), "ne".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// Comparing two `list.at(list, i)` results — where neither operand is a literal —
    /// must compare String *content* on WASM, not pointers. The list holds two
    /// runtime-built (concatenated) strings with equal content but distinct heap
    /// addresses, so a pointer comparison would wrongly report "ne". Codegen now
    /// carries a `List(String)`'s element value type to `list.at(...)`, so `==` lowers
    /// to `$str_eq`. (Regression for the run-length-encoding parity divergence.)
    #[test]
    fn wasm_string_eq_on_two_at_results_compares_content() {
        let src = "fn main(console: Console):\n    let a = \"x\" + \"y\"\n    let b = \"x\" + \"y\"\n    let xs = [a, b, \"zz\"]\n    console.print(if list.at(xs, 0) == list.at(xs, 1): \"eq\" else: \"ne\")\n    console.print(if list.at(xs, 0) == list.at(xs, 2): \"eq\" else: \"ne\")\n";
        let want = vec!["eq".to_string(), "ne".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// An *unbounded* generic function that compares its type-variable values
    /// (`x == target`) must compare String CONTENT on WASM, not pointers. The
    /// WASM backend monomorphizes the call on the concrete element type
    /// (`count_eq__String`), so `==` lowers to `$str_eq`. The strings are built at
    /// runtime (distinct pointers, equal content) so a pointer compare would give
    /// the wrong count. (Regression for the generic-`==`-on-non-primitives gap.)
    #[test]
    fn wasm_monomorphizes_generic_equality_on_strings() {
        let src = "fn count_eq(xs: List(a), target: a) -> Int:\n    var n = 0\n    for x in xs:\n        if x == target:\n            n = n + 1\n    n\n\nfn b(s: String) -> String:\n    s + \"\"\n\nfn main(console: Console):\n    console.print(\"${count_eq([b(\"aa\"), b(\"bb\"), b(\"aa\")], b(\"aa\"))}\")\n";
        let want = vec!["2".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// `string_to_int` of a value that overflows i64 must FAIL on both backends,
    /// not silently wrap on WASM. The compiled `$str_to_int` now traps once the
    /// running magnitude would exceed the sign-appropriate i64 bound (2^63-1, or
    /// 2^63 for a negative), matching Rust's checked parse. The exact boundaries
    /// (i64::MAX / i64::MIN) still parse. (Regression for a silent overflow-wrap
    /// divergence.)
    #[test]
    fn string_to_int_overflow_errors_on_both_backends() {
        let err_cases = [
            "99999999999999999999999",
            "9223372036854775808",  // i64::MAX + 1
            "-9223372036854775809", // i64::MIN - 1
        ];
        for v in err_cases {
            let src = format!(
                "fn main(console: Console):\n    console.print(\"${{\"{v}\".to_int()}}\")\n"
            );
            let module = parser::parse_module(&src).expect("parse");
            let bytes = codegen::compile_module_binary(&module)
                .expect_lowered("the binary path lowers this program");
            assert!(interpreter::run(&src).is_err(), "interpreter must error on `{v}`");
            assert!(crate::run_wasm_bytes(&bytes).is_err(), "WASM must trap on `{v}`");
        }
        // The exact i64 boundaries parse identically on both backends.
        let ok = "fn main(console: Console):\n    console.print(\"${\"9223372036854775807\".to_int()}\")\n    console.print(\"${\"-9223372036854775808\".to_int()}\")\n";
        let want = vec![
            "9223372036854775807".to_string(),
            "-9223372036854775808".to_string(),
        ];
        assert_eq!(interp(ok), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(ok), want, "compiled WASM must agree");
    }

    /// `to_string` of a builtin call result (`has` -> Bool, `size` -> Int) must
    /// compile and render the same on both backends — codegen knows these
    /// builtins' value types, so it picks the right formatter instead of erroring
    /// with "could not determine the value's type". (Regression for the
    /// call-result val-type gap that previously forced `int_to_string`/explicit
    /// conversion.)
    #[test]
    fn to_string_of_builtin_call_results_agrees() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, \"a\", 1)\n    dict.insert(d, \"b\", 2)\n    console.print(\"${dict.contains_key(d, \"a\")}\")\n    console.print(\"${dict.contains_key(d, \"z\")}\")\n    console.print(\"${dict.length(d)}\")\n    console.print(\"${\"hello\".contains(\"ell\")}\")\n";
        let want = vec![
            "true".to_string(),
            "false".to_string(),
            "2".to_string(),
            "true".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// The `encoding` module (hex/base64) must agree on both backends. WASM
    /// bridges each `String -> String` transform to the same native registry the
    /// interpreter uses (a host import), so output is byte-for-byte identical.
    /// (Regression for the interpreter-only encoding-module gap.)
    #[test]
    fn encoding_module_agrees_on_both_backends() {
        let src = "import encoding\n\nfn main(console: Console):\n    let p = \"Hello, witchy!\"\n    let b = encoding.base64_encode(p)\n    console.print(b)\n    console.print(encoding.base64_decode(b).unwrap_or(\"?\"))\n    let h = encoding.hex_encode(p)\n    console.print(h)\n    console.print(encoding.hex_decode(h).unwrap_or(\"?\"))\n    console.print(encoding.base64_encode(\"foo\"))\n";
        let want = vec![
            "SGVsbG8sIHdpdGNoeSE=".to_string(),
            "Hello, witchy!".to_string(),
            "48656c6c6f2c2077697463687921".to_string(),
            "Hello, witchy!".to_string(),
            "Zm9v".to_string(),
        ];
        // `import encoding` is a native module: link to register its signatures,
        // then run each backend on the linked module.
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        assert_eq!(link_run(src), want.clone(), "interpreter (linked)");
        assert_eq!(crate::run_wasm_bytes(&bytes).expect("wasm run"), want, "compiled WASM must agree");
    }

    /// `to_string` on a `Float` must produce the same text on both backends.
    /// WASM has no float formatter in hand-written WAT, so codegen calls a
    /// `float_to_str` host import that formats with Rust `Display` — byte-for-byte
    /// the interpreter's format. (Regression for the interpreter-only float
    /// `to_string` gap.)
    #[test]
    fn float_to_string_agrees_on_both_backends() {
        // Ordinary floats plus the IEEE special values whose rendering is most
        // likely to diverge between a Rust f64 and the compiled backend: the
        // infinities, NaN, and negative zero must format identically on both.
        let src = "fn main(console: Console):\n    console.print(\"${3.5}\")\n    console.print(\"${2.0}\")\n    console.print(\"${0.0 - 1.0 / 3.0}\")\n    console.print(\"${0.1 + 0.2}\")\n    console.print(\"${1000000.0}\")\n    console.print(\"${0.0}\")\n    console.print(\"${10.0 / 0.0}\")\n    console.print(\"${(0.0 - 10.0) / 0.0}\")\n    console.print(\"${0.0 / 0.0}\")\n    console.print(\"${(0.0 - 1.0) * 0.0}\")\n";
        let want = vec![
            "3.5".to_string(),
            "2.0".to_string(),
            "-0.3333333333333333".to_string(),
            "0.30000000000000004".to_string(),
            "1000000.0".to_string(),
            "0.0".to_string(),
            "inf".to_string(),
            "-inf".to_string(),
            "NaN".to_string(),
            "-0.0".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// `to_upper`/`to_lower` now compile to WASM (ASCII case mapping), matching
    /// the interpreter's ASCII fold byte-for-byte — no longer interpreter-only.
    #[test]
    fn wasm_ascii_case_mapping() {
        let src = "fn main(console: Console):\n    console.print(\"Hi, World! 9z\".to_upper())\n    console.print(\"Hi, World! 9A\".to_lower())\n";
        let want = vec!["HI, WORLD! 9Z".to_string(), "hi, world! 9a".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// `string_to_int` must accumulate in i64 (matching the interpreter's
    /// `parse::<i64>()`) and trim surrounding whitespace. WASM used to parse into
    /// i32, so a value past 2^31 (e.g. 5000000000) silently truncated to a wrong
    /// number; it now agrees on both backends.
    #[test]
    fn wasm_string_to_int_uses_i64_and_trims() {
        let src = "fn main(console: Console):\n    console.print(\"${\"5000000000\".to_int()}\")\n    console.print(\"${\"-7000000000\".to_int()}\")\n    console.print(\"${\"  42  \".to_int()}\")\n";
        let want = vec![
            "5000000000".to_string(),
            "-7000000000".to_string(),
            "42".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// (BUG-011) `string.substring` must clamp BOTH indices to `[0, char_count]`
    /// in full i64 width on BOTH backends. The compiled path used to narrow the
    /// i64 char index to i32 *before* clamping, so an index near the i64 extremes
    /// wrapped (a huge `end` became `< start`) and the slice came back `""` while
    /// the interpreter clamped in i64 and returned the whole string. Covers a
    /// negative `i`, `i > len`, `j > len`, `i > j`, and both i64 extremes.
    #[test]
    fn wasm_substring_clamps_out_of_range_indices_in_i64() {
        let src = r#"fn main(console: Console):
    let s = "abcdef"
    console.print(s.substring((-2), 3))
    console.print(s.substring(2, 100))
    console.print(s.substring(4, 2))
    console.print(s.substring(0, 6))
    console.print(s.substring((-9000000000), 9000000000))
    console.print(s.substring((-9223372036854775807), 9223372036854775807))
    console.print("X-5166417078869286437Y".substring((-3261219961577993898), 5500724189412945291))
"#;
        let want = vec![
            "abc".to_string(),
            "cdef".to_string(),
            String::new(),
            "abcdef".to_string(),
            "abcdef".to_string(),
            "abcdef".to_string(),
            "X-5166417078869286437Y".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    #[test]
    fn sort_strings_backends_agree() {
        // Sorting strings lexicographically with `sort_by` and a String
        // comparator — exercising string `<` through call_indirect inside
        // insert_sorted — agrees across backends.
        let client = r#"
import list

fn main(console: Console):
    var words = ["cherry", "apple", "banana", "date", "apple"]
    list.sort_by(words, fn(a: String, b: String): (a < b))
    for w in words:
        console.print(w)
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
    fn std_ascii_classification_backends_agree() {
        // ASCII predicates are implemented purely via string comparison, so they
        // must agree across the interpreter and the compiled backend. Also drives
        // a tiny tokenizer-style use: sum the digit values in a string.
        let client = r#"
import ascii

fn digit_sum(s: String) -> Int:
    var total = 0
    var i = 0
    while (i < s.char_count()):
        let c = s.char_at(i) ?? ""
        if ascii.is_digit(c):
            total = (total + (ascii.to_digit(c) ?? 0))
        i = (i + 1)
    total

fn main(console: Console):
    console.print("${ascii.is_digit("7")}")
    console.print("${ascii.is_digit("x")}")
    console.print("${ascii.is_alpha("Q")}")
    console.print("${ascii.is_alnum("_")}")
    console.print("${ascii.is_space("\t")}")
    console.print("${ascii.to_digit("4") ?? -1}")
    console.print("${ascii.to_digit("z") ?? -1}")
    console.print("${digit_sum("a1b2c3")}")
    console.print("${ascii.all_digits("12345")}")
    console.print("${ascii.all_digits("12a45")}")
    console.print("${ascii.all_digits("")}")
    console.print("${ascii.all_digits("0")}")
"#;
        let sources = [
            ("ascii", crate::bundled_module("ascii").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std ascii diverged");
        assert_eq!(
            compiled,
            vec![
                "true", "false", "true", "false", "true", "4", "-1", "6", // all_digits:
                "true", "false", "false", "true",
            ]
        );
    }

    /// `string_chars` (the O(n) string -> List(String) primitive behind a fast
    /// `to_chars`) agrees across the interpreter and WASM —
    /// including a multi-byte (UTF-8) character. Counted by Unicode scalar.
    #[test]
    fn string_chars_backends_agree() {
        let src = "fn main(console: Console):\n    let cs = \"café\".chars()\n    console.print(\"${list.length(cs)}\")\n    console.print(list.at(cs, 0))\n    console.print(list.at(cs, 3))\n";
        let expected = vec!["4".to_string(), "c".to_string(), "é".to_string()];
        // Interpreter (source of truth).
        assert_eq!(interpreter::run(src).expect("interp"), expected);
        // WASM.
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm diverged");
    }

    #[test]
    fn string_parse_int_backends_agree() {
        // parse_int validates an optional sign + digits before calling the raw
        // string_to_int builtin, so bad input is None (not a trap) consistently.
        let client = r#"
import option
fn show(o: Option(Int)) -> String:
    match o:
        Some(n) -> "${n}"
        None -> "none"
fn main(console: Console):
    console.print(show("42".parse_int()))
    console.print(show("-7".parse_int()))
    console.print(show("0".parse_int()))
    console.print(show("".parse_int()))
    console.print(show("-".parse_int()))
    console.print(show("12a".parse_int()))
    console.print(show("3.5".parse_int()))
    console.print(show(" 5".parse_int()))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "parse_int diverged");
        assert_eq!(
            compiled,
            vec!["42", "-7", "0", "none", "none", "none", "none", "none"]
        );
    }

    #[test]
    fn string_center_backends_agree() {
        // center pads both sides; an odd remainder goes on the right, and a
        // string already at/over width is returned unchanged.
        let client = r#"
fn main(console: Console):
    console.print("[" + "hi".center(6, " ") + "]")
    console.print("[" + "hi".center(7, " ") + "]")
    console.print("[" + "odd".center(8, "*") + "]")
    console.print("[" + "toolong".center(4, " ") + "]")
    console.print("[" + "x".center(1, " ") + "]")
"#;
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "center diverged");
        assert_eq!(
            compiled,
            vec!["[  hi  ]", "[  hi   ]", "[**odd***]", "[toolong]", "[x]"]
        );
    }

    #[test]
    fn url_format_roundtrip_backends_agree() {
        // format is parse's inverse; the default port is omitted, a non-default
        // port is kept, and an absent path renders as "/".
        let client = r#"
import url
import result
fn render(s: String) -> String:
    match url.parse(s):
        Ok(u) -> url.format(u)
        Err(_e) -> "no parse"
fn main(console: Console):
    console.print(render("https://example.com/path"))
    console.print(render("http://example.com:8080/x"))
    console.print(render("ftp://host:21/file"))
    console.print(render("http://example.com"))
    console.print(render("not a url"))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("url", crate::bundled_module("url").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "url.format diverged");
        assert_eq!(
            compiled,
            vec![
                "https://example.com/path",
                "http://example.com:8080/x",
                "ftp://host:21/file",
                "http://example.com/",
                "no parse",
            ]
        );
    }

    #[test]
    fn url_parse_rejects_bad_port_without_trapping_backends_agree() {
        // A non-decimal or empty `:port` makes parse return None — it used to trap
        // in string_to_int. Signs accepted by the general integer parser are not
        // URL port syntax. A valid or defaulted port still parses, both backends.
        let client = r#"
import url
import result
fn p(s: String) -> String:
    match url.parse(s):
        Ok(u) -> "ok:" + "${url.port(u)}"
        Err(_e) -> "none"
fn main(console: Console):
    console.print(p("https://h:8443/x"))
    console.print(p("https://h:abc/x"))
    console.print(p("https://h:/x"))
    console.print(p("https://h:80x/x"))
    console.print(p("https://h:+80/x"))
    console.print(p("https://h:-0/x"))
    console.print(p("https://h/x"))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("url", crate::bundled_module("url").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "url bad-port diverged");
        assert_eq!(
            compiled,
            vec!["ok:8443", "none", "none", "none", "none", "none", "ok:443"]
        );
    }

    /// (BUG-470) `url.decode` percent-decodes path components (+ stays literal),
    /// `url.decode_form` also maps + to space (query/form convention). Both handle
    /// multi-byte UTF-8 escapes and stray `%` passthrough. Parity on both backends.
    #[test]
    fn url_decode_and_decode_form_backends_agree() {
        let client = r#"
import url
fn main(console: Console):
    // Basic ASCII escapes
    console.print(url.decode("hello%20world"))
    // Multi-byte UTF-8 (€ = E2 82 AC)
    console.print(url.decode("%E2%82%AC"))
    // + stays literal in path mode
    console.print(url.decode("a+b"))
    // + becomes space in form mode
    console.print(url.decode_form("a+b"))
    // Mixed: encoded and plain
    console.print(url.decode_form("key%3D%26val+ue"))
    // Stray % passes through
    console.print(url.decode("100%"))
    // encode/decode round-trip
    console.print(url.decode(url.encode("hello world/€")))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("url", crate::bundled_module("url").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "url.decode diverged");
        assert_eq!(
            compiled,
            vec![
                "hello world",
                "€",
                "a+b",
                "a b",
                "key=&val ue",
                "100%",
                "hello world/€",
            ]
        );
    }

    #[test]
    fn url_parse_ipv6_and_userinfo_backends_agree() {
        // A bracketed IPv6 authority keeps its inner colons in the host and splits
        // the port at the colon after `]` — matching the Net layer's last-colon /
        // bracket-aware split (BUG-351). Userinfo (`user@`, `user:pass@`) is outside
        // this minimal grammar and is rejected loudly rather than reinterpreted as
        // host/port text (BUG-380), and an empty bracketed literal is malformed.
        // Both backends agree, and format round-trips.
        let client = r#"
import url
import result
fn p(s: String) -> String:
    match url.parse(s):
        Ok(u) -> url.host(u) + " " + "${url.port(u)}" + " " + url.format(u)
        Err(_e) -> "err"
fn main(console: Console):
    console.print(p("http://[::1]:8080/x"))
    console.print(p("http://[::1]/x"))
    console.print(p("https://[2001:db8::1]:443/y"))
    console.print(p("http://[]/x"))
    console.print(p("https://user@example.com/x"))
    console.print(p("https://user:pass@example.com/x"))
    console.print(p("https://example.com:8443/z"))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("url", crate::bundled_module("url").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "url ipv6/userinfo diverged");
        assert_eq!(
            compiled,
            vec![
                "[::1] 8080 http://[::1]:8080/x",
                "[::1] 80 http://[::1]/x",
                "[2001:db8::1] 443 https://[2001:db8::1]/y",
                "err",
                "err",
                "err",
                "example.com 8443 https://example.com:8443/z",
            ]
        );
    }

    #[test]
    fn string_rsplit_once_backends_agree() {
        // rsplit_once splits on the LAST separator (vs split_once's first); when
        // the separator is absent the whole string is the right part.
        let client = r#"
fn show2(p: (String, String)) -> String:
    let (a, b) = p
    a + "|" + b
fn main(console: Console):
    console.print(show2("a.b.c".rsplit_once(".")))
    console.print(show2("a.b.c".split_once(".")))
    console.print(show2("nodot".rsplit_once(".")))
    console.print(show2("file.tar.gz".rsplit_once(".")))
    console.print("${"a.b.c".last_index_of(".") ?? -1}")
    console.print("${"nodot".last_index_of(".") ?? -1}")
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "rsplit_once diverged");
        assert_eq!(
            compiled,
            vec!["a.b|c", "a|b.c", "|nodot", "file.tar|gz", "3", "-1"]
        );
    }

    #[test]
    fn std_ord_string_and_sort_backends_agree() {
        // `impl Ord for String` makes strings comparable, and the bounded generic
        // `list.sort` dispatches through the element's Ord impl — so it sorts
        // runtime-BUILT strings content-correctly on both backends (a pointer
        // comparison sort would scramble them in compiled code). Also covers
        // Ord-over-String for max_of/maximum and Ints via the same `sort`.
        let client = r#"
import cmp

fn build(s: String) -> String:
    var acc = ""
    var i = 0
    while (i < s.char_count()):
        acc = (acc + s.substring(i, (i + 1)))
        i = (i + 1)
    acc

fn main(console: Console):
    var words = [build("pear"), build("apple"), build("fig"), build("apple")]
    list.sort(words)
    console.print(list.join(words, ","))
    var letters = ["c", "a", "b"]
    list.sort(letters)
    console.print(list.join(letters, ""))
    console.print(cmp.max_of(build("alpha"), build("omega")))
    console.print(cmp.maximum([build("x"), build("a"), build("m")], ""))
    var nums = [3, 1, 2, 1]
    list.sort(nums)
    console.print("${(list.at(nums, 0) + (list.at(nums, 3) * 10))}")
"#;
        let sources = [
            ("cmp", crate::bundled_module("cmp").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std ord string/sort diverged");
        assert_eq!(
            compiled,
            vec!["apple,apple,fig,pear", "abc", "omega", "x", "31"]
        );
    }

    /// The first host-import helper ($encoding) on the binary path. Kept out of
    /// the corpus above because `encoding.*` requires `import encoding`, which the
    /// corpus's `run_on_wasm`/`typeck::check_str` leg can't resolve (it doesn't
    /// pull in std modules); the linked interpreter oracle (`link_run`) can. So we
    /// compare the pruned binary against the interpreter directly. The pruned
    /// module must import "encoding" alongside "print".
    #[test]
    fn wir_encoding_host_import_binary_path() {
        let src = "import encoding\nfn main(console: Console):\n    console.print(encoding.hex_encode(\"Hi\"))\n    console.print(encoding.base64_encode(\"Hi\"))\n";
        let want = vec!["4869".to_string(), "SGk=".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should handle encoding via the host import");
        // AST → WIR → binary runs identically to the interpreter oracle, under a
        // print-only grant (proving the pruned module imports only print+encoding,
        // and that `encoding` is host-provided regardless of the grant).
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path");
        assert_eq!(link_run(src), want, "interpreter oracle");
    }

    #[test]
    fn split_runs_on_wasm() {
        // `split` compiled to WASM, matching Rust's str::split: pieces between
        // separators, empty pieces kept, multi-char separators, and an empty
        // separator yielding the whole string.
        let src = r#"
fn main(console: Console):
    let p = "a,bb,ccc".split(",")
    console.print("${list.length(p)}")
    console.print(list.at(p, 0))
    console.print(list.at(p, 2))
    console.print("${list.length("a,,b".split(","))}")
    console.print(list.at("a,,b".split(","), 1))
    console.print("${list.length("".split(","))}")
    console.print("${list.length("abc".split(""))}")
    console.print(list.at("xXXyXXz".split("XX"), 2))
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
fn classify(n: Int) -> Bool:
    (n > 0)

fn main(console: Console):
    console.print("${42}")
    console.print("${(0 - 5)}")
    console.print("${true}")
    console.print("${(3 > 7)}")
    console.print("${"hi"}")
    console.print("${classify(9)}")
    let flag = (2 == 2)
    console.print("${flag}")
"#;
        assert_eq!(
            run_on_wasm(src),
            vec!["42", "-5", "true", "false", "hi", "true", "true"]
        );
    }

    #[test]
    fn to_string_on_compound_renders_on_wasm() {
        // A compound (list/tuple/record/ADT/dict, any nesting) renders byte-
        // identically to the interpreter via a generated per-shape helper — so
        // `to_string`/`${...}` work on WASM, not just the interpreter.
        let src = r#"
type Shape:
    Circle(Int)
    Dot

fn main(console: Console):
    console.print("${[1, 2, 3]}")
    console.print("${[[1, 2], [3]]}")
    console.print("${(1, "two", true)}")
    console.print("${[Circle(2), Dot]}")
    var d = dict.new()
    dict.insert(d, "a", 1)
    dict.insert(d, "b", 2)
    console.print("${d}")
    let tc = ([1, 2], (3, 4))          // a let-bound tuple whose slots are compound
    console.print("${tc}")
"#;
        assert_eq!(
            run_on_wasm(src),
            vec![
                "[1, 2, 3]",
                "[[1, 2], [3]]",
                "(1, two, true)",
                "[Circle(2), Dot]",
                "{a: 1, b: 2}",
                "([1, 2], (3, 4))",
            ]
        );
    }

    #[test]
    fn to_string_through_generics_renders() {
        // Typed lowering (Phase 0) resolves what used to be undetermined: a
        // generic tuple rendered through a monomorphizable call works
        // identically on both backends. (The loud could-not-determine error
        // remains for shapes with NO resolvable call site.)
        let src = r#"
fn render(t: (a, a)) -> String:
    "${t}"

fn main(console: Console):
    console.print(render((1, 2)))
"#;
        assert_eq!(link_run(src), vec!["(1, 2)"], "interpreter");
        assert_eq!(wasm_run(src), vec!["(1, 2)"], "wasm");
    }

    #[test]
    fn negative_int_to_string_on_wasm() {
        // `int_to_string` renders negatives with a leading '-' (previously it
        // emitted garbage, e.g. "/" for -1).
        let src = r#"
fn main(console: Console):
    console.print("${(0 - 1)}")
    console.print("${(0 - 128)}")
    console.print("${255}")
    console.print("${0}")
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
fn main(console: Console):
    console.print("a,b,c".replace(",", ";"))
    console.print("aXXbXXc".replace("XX", "-"))
    console.print("aaa".replace("aa", "x"))
    console.print("a,b,c".replace(",", ""))
    console.print("abc".replace("b", "XYZ"))
    console.print("abc".replace("z", "Q"))
    console.print("ab".replace("", "-"))
    console.print("café".replace("é", "e"))
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
        // 4 (byte 5), and string.substring(3,5) is the two characters "é!".
        let src = r#"
fn main(console: Console):
    console.print("${if "hello world".contains("world"): 1 else: 0}")
    console.print("${if "abc".contains("xyz"): 1 else: 0}")
    console.print("${if "abc".contains(""): 1 else: 0}")
    console.print("${"hello".contains("l")}")
    console.print("${"hello".contains("z")}")
    console.print("hello".substring(1, 4))
    console.print("hi".substring(0, 100))
    console.print("hi".substring(5, 10))
    console.print("${"café!".contains("!")}")
    console.print("café!".substring(3, 5))
"#;
        assert_eq!(
            run_on_wasm(src),
            vec!["1", "0", "1", "true", "false", "ell", "hi", "", "true", "é!"]
        );
    }

    #[test]
    fn dict_string_keys_on_wasm() {
        // String-keyed Dict compiled to WASM: insert (append + replace), get_or
        // (present/absent), has, and size — keys compared with $str_eq.
        let src = r#"
fn main(console: Console):
    var d = dict.new()
    dict.insert(d, "a", 1)
    dict.insert(d, "b", 2)
    dict.insert(d, "a", 10)
    console.print("${dict.get_or(d, "a", 0)}")
    console.print("${dict.get_or(d, "b", 0)}")
    console.print("${dict.get_or(d, "z", (0 - 1))}")
    console.print("${dict.length(d)}")
    console.print("${if dict.contains_key(d, "b"): 1 else: 0}")
    console.print("${if dict.contains_key(d, "q"): 1 else: 0}")
"#;
        assert_eq!(run_on_wasm(src), vec!["10", "2", "-1", "2", "1", "0"]);
    }

    #[test]
    fn std_string_compiles_and_runs_on_wasm() {
        // With `split` compiled, the whole `string` module compiles: `lines`
        // (split on "\n"), `join`, and `repeat`. lines -> ["a","bb","ccc"] (3);
        // join -> "a-bb-ccc" (8); repeat -> "zzzzz" (5): 3*100 + 8 + 5 = 313.
        let client = r#"

fn main() -> Int:
    let parts = "a\nbb\nccc".lines()
    let joined = list.join(parts, "-")
    let r = "z".repeat(5)
    (((list.length(parts) * 100) + joined.length()) + r.length())
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

fn main(console: Console):
    console.print("42".pad_left(5, "0"))
    console.print("42".pad_right(5, "."))
    console.print("hello".pad_left(3, "x"))
    console.print("ab".pad_left(7, "-="))
    console.print("café".pad_left(6, "*"))
    console.print("café".pad_right(6, "*"))
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "pad diverged between backends");
        // Widths are by character: "café" is 4 chars, so pad to 6 adds two stars
        // (a byte-based width would have added only one).
        assert_eq!(
            compiled,
            vec!["00042", "42...", "hello", "-=-=-ab", "**café", "café**"]
        );
    }

    #[test]
    fn std_string_strip_backends_agree() {
        // strip_prefix/strip_suffix remove an affix only when it matches,
        // leaving the string untouched otherwise; stripping the whole string
        // yields "". Complements starts_with/ends_with.
        let client = r#"

fn main(console: Console):
    console.print("witchy.lang".strip_prefix("witchy."))
    console.print("witchy.lang".strip_prefix("scala."))
    console.print("main.witchy".strip_suffix(".witchy"))
    console.print("main.rs".strip_suffix(".witchy"))
    console.print("abc".strip_prefix("abc"))
    console.print("émile".strip_prefix("é"))
    console.print("héllo!".strip_suffix("!"))
    console.print("naïveté".strip_suffix("té"))
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "strip diverged between backends");
        // The multibyte rows pin the char-count fix: the old bodies mixed
        // string.length (bytes) into substring's character offsets, so any
        // multibyte affix ate extra chars (prefix) or disabled the strip
        // entirely (suffix).
        assert_eq!(
            compiled,
            vec!["lang", "witchy.lang", "main", "main.rs", "", "mile", "héllo", "naïve"]
        );
    }

    #[test]
    fn large_string_concat_grows_memory() {
        // Concatenating a 400-char string one char at a time allocates ~80KB of
        // intermediate strings — past the initial page — and must grow.
        let src = r#"
fn main() -> Int:
    var s = ""
    var i = 0
    while (i < 400):
        s = (s + "x")
        i = (i + 1)
    s.length()
"#;
        assert_eq!(run_on_wasm(src), vec!["400"]);
    }

    #[test]
    fn string_prefix_suffix_on_wasm() {
        // starts_with / ends_with compile to byte-loop helpers.
        // check("html")=2, check("http")=1, check("xml")=0 -> 210.
        let src = r#"
fn check(s: String) -> Int:
    if s.starts_with("ht"):
        if s.ends_with("ml"):
            2
        else:
            1
    else:
        0

fn main() -> Int:
    (((check("html") * 100) + (check("http") * 10)) + check("xml"))
"#;
        assert_eq!(run_on_wasm(src), vec!["210"]);
    }

    #[test]
    fn string_interpolation_backends_agree() {
        // `${expr}` desugars through generated render + concat, so interpolation works
        // in both backends: String pass-through, Int/Bool via to_string, embedded
        // calls/arithmetic, `\$` for a literal `$`, and adjacent interpolations.
        let src = r#"
fn main(console: Console):
    let name = "witchy"
    let age = 3
    console.print("hi ${name}, age ${age}")
    console.print("sum: ${"${age + 10}"}")
    console.print("flag ${age > 1}")
    console.print("literal \${x} stays")
    console.print("${name}${name}")
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

    // `replace` with an empty `from` is a notorious edge (the interpreter's
    // Rust `str::replace` inserts the replacement around every character);
    // Int-keyed dicts exercise the by-value key-comparison path. Both must
    // match the compiled backend exactly. Agreement-only, so a future divergence
    // is caught without baking in a hand-computed expectation.
    #[test]
    fn replace_and_int_keyed_dict_backends_agree() {
        let src = r#"
fn main(console: Console):
    console.print((("[" + "abc".replace("", "-")) + "]"))
    console.print("abc".replace("x", "y"))
    console.print("aaa".replace("a", "bb"))
    console.print("hello world".replace("o", "0"))
    var d = dict.new()
    dict.insert(d, 1, 100)
    dict.insert(d, 2, 200)
    dict.insert(d, 1, 111)
    console.print("${dict.get_or(d, 1, 0)}")
    console.print("${dict.get_or(d, 2, 0)}")
    console.print("${dict.length(d)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "replace/int-key dict diverged");
    }

    // char_count returns Unicode scalars; string_length returns bytes. They
    // agree for ASCII and diverge for multi-byte UTF-8 ("café" is 4 chars, 5
    // bytes) — and both backends must compute each identically.
    #[test]
    fn char_count_vs_string_length_backends_agree() {
        let src = r#"
fn main(console: Console):
    console.print("${"hello".char_count()}")
    console.print("${"hello".length()}")
    console.print("${"café".char_count()}")
    console.print("${"café".length()}")
    console.print("${"".char_count()}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "char_count diverged");
        assert_eq!(run_on_wasm(src), vec!["5", "5", "4", "5", "0"]);
    }

    #[test]
    fn substring_is_char_indexed_across_multibyte_on_both_backends() {
        // substring indexes by CHARACTER, not byte: slicing across a 2-byte (é)
        // or 4-byte (emoji) boundary must compute the same char->byte offsets on
        // both backends, while length (bytes) vs char_count tracks UTF-8 widths.
        let src = r#"
fn main(console: Console):
    console.print("café".substring(0, 3))
    console.print("café".substring(3, 4))
    console.print("${"a😀b".length()}")
    console.print("${"a😀b".char_count()}")
    console.print("a😀b".substring(1, 2))
    console.print("a😀b".substring(0, 2))
"#;
        assert_eq!(interp(src), run_on_wasm(src), "multibyte substring diverged");
        assert_eq!(run_on_wasm(src), vec!["caf", "é", "6", "3", "😀", "a😀"]);
    }

    // reverse flips character order using char_count + char-based substring, so
    // it's correct for multi-byte UTF-8 ("café" -> "éfac"), not just ASCII.
    // Char-based take/drop: clamp at the ends and count by Unicode scalar, so
    // they slice "café" correctly (take 2 -> "ca", drop 3 -> "é").
    #[test]
    fn std_string_take_drop_backends_agree() {
        let client = r#"

fn main(console: Console):
    console.print("hello".take(3))
    console.print((("[" + "hi".take(10)) + "]"))
    console.print((("[" + "hi".take(0)) + "]"))
    console.print("hello".drop(2))
    console.print((("[" + "hi".drop(5)) + "]"))
    console.print("café".take(2))
    console.print("café".drop(3))
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

fn main(console: Console):
    console.print("hello".reverse())
    console.print((("[" + "".reverse()) + "]"))
    console.print("a".reverse())
    console.print("café".reverse())
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

fn main(console: Console):
    console.print("a.b.c".replace_first(".", "/"))
    console.print("hello".replace_first("l", "L"))
    console.print("xyz".replace_first("q", "Q"))
    console.print("aa".replace_first("a", "bb"))
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

fn main(console: Console):
    let (k, v) = "name=witchy".split_once("=")
    console.print(k)
    console.print(v)
    let (a, b) = "no-sep-here".split_once("=")
    console.print(a)
    console.print((("[" + b) + "]"))
    let (h, rest) = "a=b=c".split_once("=")
    console.print(h)
    console.print(rest)
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

fn main(console: Console):
    let ws = "the  quick\tbrown\nfox ".words()
    console.print("${list.length(ws)}")
    for w in ws:
        console.print(w)
    console.print("${list.length("   ".words())}")
    console.print("${list.length("".words())}")
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

fn main(console: Console):
    let cs = "café".chars()
    console.print("${list.length(cs)}")
    for c in cs:
        console.print(c)
    console.print("${list.length("".chars())}")
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

fn main(console: Console):
    console.print("${"".is_empty()}")
    console.print("${"x".is_empty()}")
    console.print("${"banana".count("a")}")
    console.print("${"banana".count("an")}")
    console.print("${"aaaa".count("aa")}")
    console.print("${"abc".count("x")}")
    console.print("${"abc".count("")}")
    console.print("${"aéaéa".count("éa")}")
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "string is_empty/count diverged");
        // The last counts a multi-byte needle: "éa" occurs twice in "aéaéa" —
        // a byte-based advance would miscount it (and matters only off ASCII).
        assert_eq!(compiled, vec!["true", "false", "3", "2", "2", "0", "0", "2"]);
    }

    #[test]
    fn std_string_char_at_backends_agree() {
        // RFC-0044 rule 1: char_at returns `Some(c)` in range, `None` out of range
        // (no more "" sentinel). `?? "?"` recovers a display char for the miss.
        let client = r#"

fn main(console: Console):
    console.print("witchy".char_at(0) ?? "?")
    console.print("witchy".char_at(5) ?? "?")
    console.print((("[" + ("witchy".char_at(10) ?? "")) + "]"))
    console.print((("[" + ("".char_at(0) ?? "")) + "]"))
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
fn put(var d: Dict(String, Int), k: String, v: Int) -> Nil:
    dict.insert(d, k, v)
    return

fn lookup(d: Dict(String, Int), k: String) -> Int:
    dict.get_or(d, k, (0 - 1))

fn main(console: Console):
    var d = dict.new()
    put(d, "apple", 1)
    put(d, "banana", 2)
    console.print("${lookup(d, ("ap" + "ple"))}")
    console.print("${lookup(d, "banana")}")
    console.print("${lookup(d, "cherry")}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "dict string-key via helpers diverged");
        assert_eq!(run_on_wasm(src), vec!["1", "2", "-1"]);
    }

    #[test]
    fn string_edge_cases_backends_agree() {
        let src = r#"
fn main(console: Console):
    console.print("${list.length("abc".split(""))}")
    console.print("${list.length("abc".split("x"))}")
    console.print("${list.length("a,b,c".split(","))}")
    console.print((("[" + "".substring(0, 5)) + "]"))
    console.print((("[" + "hello".substring(3, 1)) + "]"))
    console.print("hello".substring(2, 100))
    console.print("${"hello".contains("")}")
    console.print("${"hello".contains("z")}")
    console.print((("[" + (("" + "x") + "")) + "]"))
    console.print("${"".length()}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "string edge cases diverged");
    }

    // std/url: parse assorted URL strings (default ports, explicit port, path,
    // and a malformed one). Pure, so both backends agree.
    #[test]
    fn std_url_parse_backends_agree() {
        let client = r#"
import url
fn describe(s: String) -> String:
    match url.parse(s):
        Ok(u) -> url.scheme(u) + " " + url.host(u) + " " + "${url.port(u)}" + " " + url.path(u)
        Err(e) -> "invalid: " + url.url_error_message(e)
fn main(console: Console):
    console.print(describe("http://example.com"))
    console.print(describe("http://example.com:8080/foo"))
    console.print(describe("https://x.com/a/b"))
    console.print(describe("notaurl"))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std url parse diverged");
        assert_eq!(
            compiled,
            vec![
                "http example.com 80 /",
                "http example.com 8080 /foo",
                "https x.com 443 /a/b",
                "invalid: missing `scheme://` in: notaurl"
            ]
        );
    }

    // std/http get_url: parse a URL string and GET it (loopback). Interpreter-only.
    // std/string trimming: trim/trim_start/trim_end over assorted whitespace.
    // Pure, so both backends agree.
    #[test]
    fn std_string_trim_backends_agree() {
        let client = r#"
fn main(console: Console):
    console.print("[" + "  hello  ".trim() + "]")
    console.print("[" + "  hi".trim_start() + "]")
    console.print("[" + "bye  ".trim_end() + "]")
    console.print("[" + "\t\n x \r\n".trim() + "]")
    console.print("[" + "nospace".trim() + "]")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std string trim diverged");
        assert_eq!(compiled, vec!["[hello]", "[hi]", "[bye]", "[x]", "[nospace]"]);
    }

    // std/http: a real HTTP/1.1 GET over the Net capability against a loopback
    // server. Networking is interpreter-only (not compiled), so this isn't a
    // differential test; it proves the capability-gated socket primitives plus
    // the http library parse a live response into status + body.
    // A server replying with a non-numeric status code must not crash the client:
    // `status_code` guards `string_to_int` and reports 0 for a malformed status
    // line, so the body is still readable. Interpreter-only.
    // std/http POST: send a request body and read it back from a loopback echo
    // server. Interpreter-only (networking isn't compiled).
    // std/http response headers: case-insensitive lookup + a missing header.
    // Interpreter-only (networking).
    // std/json: build a nested Json value and serialize it. Pure (no
    // capabilities), so it compiles to WASM and both backends must agree.
    // std/json decode: parse JSON text then re-encode it. The round trip
    // exercises the recursive-descent parser (objects, arrays, strings, bools,
    // null, negative ints, nesting) and must agree on both backends.
    // std/json accessors: decode then pull out a string field (object key
    // lookup), an int field, and an array element. Object lookup compares the
    // decoded, heap-built key with `==`; both backends agree now that codegen
    // tracks the type of a tuple-destructured loop variable (so the comparison
    // is by content, not pointer).
    // Hex (0x..) and binary (0b..) integer literals, including underscore
    // separators, feeding the bitwise operators. Both backends agree.
    #[test]
    fn hex_binary_literals_backends_agree() {
        let src = r#"
fn main(console: Console):
    console.print("${255}")
    console.print("${10}")
    console.print("${(255 & 15)}")
    console.print("${(12 | 3)}")
    console.print("${65535}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "hex/binary literals diverged");
        assert_eq!(run_on_wasm(src), vec!["255", "10", "15", "15", "65535"]);
    }

    #[test]
    fn string_to_int_backends_agree() {
        // string_to_int now compiles: leading whitespace and an optional sign
        // are honored, and the parsed value feeds straight into arithmetic.
        let src = r#"
fn main(console: Console):
    console.print("${"42".to_int()}")
    console.print("${"-17".to_int()}")
    console.print("${"  123  ".to_int()}")
    console.print("${"+8".to_int()}")
    console.print("${"0".to_int()}")
    console.print("${("1000000".to_int() + 1)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["42", "-17", "123", "8", "0", "1000001"]);
    }

    #[test]
    fn strings_example_compiles() {
        assert_fn_compiles(include_str!("../../examples/strings/src/strings.witchy"));
    }

    /// std/url: malformed URLs return `Err` identically on both backends rather
    /// than accepting a blank scheme/host (BUG-187), swallowing a query into the
    /// host (BUG-249), or trapping on an oversized port (BUG-197).
    #[test]
    fn url_parse_rejects_malformed_on_both_backends() {
        let src = "import url\n\
                   fn show(label: String, s: String, console: Console):\n\
                   \x20   match url.parse(s):\n\
                   \x20       Ok(u) -> console.print(label + \": \" + url.scheme(u) + \"|\" + url.host(u) + \"|${url.port(u)}|\" + url.path(u))\n\
                   \x20       Err(e) -> console.print(label + \": ERR\")\n\
                   fn main(console: Console):\n\
                   \x20   show(\"empty_scheme\", \"://host\", console)\n\
                   \x20   show(\"empty_host\", \"https:///path\", console)\n\
                   \x20   show(\"query\", \"https://example.com?x=1\", console)\n\
                   \x20   show(\"big_port\", \"https://host:99999999999999999999999/p\", console)\n\
                   \x20   show(\"bad_port\", \"https://host:abc/x\", console)\n\
                   \x20   show(\"ok\", \"https://example.com/a/b\", console)\n";
        let expected = [
            "empty_scheme: ERR",
            "empty_host: ERR",
            "query: https|example.com|443|?x=1",
            "big_port: ERR",
            "bad_port: ERR",
            "ok: https|example.com|443|/a/b",
        ];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(wasm_run(src), expected, "wasm");
    }

    /// std/encoding: base64/base64url reject malformed `=` padding (a middle `=`,
    /// three `=`, an incomplete final group) rather than silently accepting it
    /// (BUG-198), and `hex_to_base64url` is fallible on non-hex input instead of
    /// silently dropping bytes (BUG-201). Both backends agree.
    #[test]
    fn encoding_rejects_malformed_padding_and_hex_on_both_backends() {
        let src = "import encoding\n\
                   fn show(label: String, r: Result(String, encoding.EncodingError), console: Console):\n\
                   \x20   match r:\n\
                   \x20       Ok(v) -> console.print(label + \": OK\")\n\
                   \x20       Err(e) -> console.print(label + \": ERR\")\n\
                   fn main(console: Console):\n\
                   \x20   show(\"mid_pad\", encoding.base64_decode(\"S=Gk\"), console)\n\
                   \x20   show(\"tail_after_pad\", encoding.base64_decode(\"ab=c\"), console)\n\
                   \x20   show(\"triple_pad\", encoding.base64_decode(\"ab===\"), console)\n\
                   \x20   show(\"pad_ok\", encoding.base64_decode(\"SGk=\"), console)\n\
                   \x20   show(\"nopad_ok\", encoding.base64_decode(\"SGk\"), console)\n\
                   \x20   show(\"url_mid_pad\", encoding.base64url_decode(\"J=Gk\"), console)\n\
                   \x20   show(\"url_ok\", encoding.base64url_decode(\"SGk\"), console)\n\
                   \x20   show(\"bad_hex\", encoding.hex_to_base64url(\"zz\"), console)\n\
                   \x20   show(\"good_hex\", encoding.hex_to_base64url(\"4869\"), console)\n";
        let expected = [
            "mid_pad: ERR",
            "tail_after_pad: ERR",
            "triple_pad: ERR",
            "pad_ok: OK",
            "nopad_ok: OK",
            "url_mid_pad: ERR",
            "url_ok: OK",
            "bad_hex: ERR",
            "good_hex: OK",
        ];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(wasm_run(src), expected, "wasm");
    }
