use super::*;
use crate::codegen;

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
            assert_eq!(
                run_linked_on_wasm(&[("main", &src)], "main"),
                ["err"],
                "WASM must reject non-hex: {bad}"
            );
        }
        // Valid hex still decodes identically on both backends.
        let ok = prog("encoding.hex_decode(\"6869\")");
        assert_eq!(run_linked_on_wasm(&[("main", &ok)], "main"), ["hi"], "wasm decodes valid hex");
    }

    /// The formatter ROUND-TRIPS string interpolation. The lexer desugars it to
    /// a generated render chain, and `interpolation_sugar` prints that AST back
    /// to the public interpolation spelling.
    #[test]
    fn fmt_round_trips_interpolation() {
        let src = "fn main(console: Console):\n    let n = 3\n    console.print(\"n is ${n}, doubled ${n * 2}\")\n    console.print(\"cost: \\$${n}\")\n";
        assert_eq!(crate::format::reformat(src).as_deref(), Some(src), "interpolation must round-trip");
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
            assert!(crate::run_admitted_artifact(&admitted_artifact(&src)).is_err(), "WASM must trap on `{v}`");
        }
        // The exact i64 boundaries parse identically on both backends.
        let ok = "fn main(console: Console):\n    console.print(\"${\"9223372036854775807\".to_int()}\")\n    console.print(\"${\"-9223372036854775808\".to_int()}\")\n";
        let want = vec![
            "9223372036854775807".to_string(),
            "-9223372036854775808".to_string(),
        ];
        assert_eq!(run_on_wasm(ok), want, "compiled WASM must agree");
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
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
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
        let compiled = run_linked_on_wasm(&sources, "main");
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
    fn substring_takes_byte_offsets_on_character_boundaries() {
        // Offsets are bytes: `é` and `ö` are two bytes, `😀` four. Offsets from
        // `index_of`, `length` and `char_offset` always land on boundaries.
        let src = r#"
fn main(console: Console):
    let s = "héllo wörld"
    console.print(s.substring(7, s.length()))
    console.print(s.substring(1, 3))
    console.print("[" + s.substring(3, 3) + "]")
    console.print(s.substring(0, s.index_of(" ") ?? 0))
    console.print("a😀b".substring(1, 5))
    console.print("${"a😀b".length()} ${"a😀b".char_count()} ${"a😀b".char_offset(2)}")
"#;
        assert_eq!(run_on_wasm(src), vec!["wörld", "é", "[]", "héllo", "😀", "6 3 5"]);
    }

    #[test]
    fn substring_aborts_off_a_boundary_out_of_range_or_inverted() {
        let compile = |src: &str| -> witchy_wir::AdmittedCanonicalArtifact {
            let checked = witchy::resolve_std_only_checked(src).expect("check");
            codegen::compile_checked_module_artifact(&checked).expect_lowered("lowers")
        };
        for span in ["0, 2", "2, 3", "0, 14", "14, 14", "3, 1", "0 - 1, 1"] {
            let prog = format!("fn main(console: Console):\n    console.print(\"héllo wörld\".substring({span}))\n");
            assert!(crate::run_admitted_artifact(&compile(&prog)).is_err(), "wasm must trap: {span}");
        }
    }

    // reverse flips character order over `chars`, so it's correct for
    // multi-byte UTF-8 ("café" -> "éfac"), not just ASCII.
    // Char-based take/drop: clamp at the ends and count by Unicode scalar, so
    // they slice "café" correctly (take 2 -> "ca", drop 3 -> "é").
    #[test]
    fn std_string_transformations_backends_agree() {
        let client = r#"

fn prefix_suffix(s: String) -> Int:
    if s.starts_with("ht"):
        if s.ends_with("ml"):
            2
        else:
            1
    else:
        0

fn main(console: Console):
    console.print("hello".take(3))
    console.print((("[" + "hi".take(10)) + "]"))
    console.print((("[" + "hi".take(0)) + "]"))
    console.print("hello".drop(2))
    console.print((("[" + "hi".drop(5)) + "]"))
    console.print("café".take(2))
    console.print("café".drop(3))
    console.print("hello".reverse())
    console.print("[" + "".reverse() + "]")
    console.print("café".reverse())
    console.print("a.b.c".replace_first(".", "/"))
    console.print("hello".replace_first("l", "L"))
    console.print("xyz".replace_first("q", "Q"))
    let (k, v) = "name=witchy".split_once("=")
    console.print(k)
    console.print(v)
    let (a, b) = "no-sep-here".split_once("=")
    console.print(a)
    console.print("[" + b + "]")
    let (h, rest) = "a=b=c".split_once("=")
    console.print(h)
    console.print(rest)
    let ws = "the  quick\tbrown\nfox ".words()
    console.print("${list.length(ws)}")
    for w in ws:
        console.print(w)
    let cs = "café".chars()
    console.print("${list.length(cs)}")
    for c in cs:
        console.print(c)
    console.print("${list.length(\"\".chars())}")
    console.print("42".pad_left(5, "0"))
    console.print("42".pad_right(5, "."))
    console.print("hello".pad_left(3, "x"))
    console.print("ab".pad_left(7, "-="))
    console.print("café".pad_left(6, "*"))
    console.print("café".pad_right(6, "*"))
    console.print("witchy.lang".strip_prefix("witchy."))
    console.print("witchy.lang".strip_prefix("scala."))
    console.print("main.witchy".strip_suffix(".witchy"))
    console.print("main.rs".strip_suffix(".witchy"))
    console.print("abc".strip_prefix("abc"))
    console.print("émile".strip_prefix("é"))
    console.print("héllo!".strip_suffix("!"))
    console.print("naïveté".strip_suffix("té"))
    console.print("${prefix_suffix(\"html\") * 100 + prefix_suffix(\"http\") * 10 + prefix_suffix(\"xml\")}")
    console.print("${\"\".is_empty()}")
    console.print("${\"x\".is_empty()}")
    console.print("${\"banana\".count(\"a\")}")
    console.print("${\"banana\".count(\"an\")}")
    console.print("${\"aaaa\".count(\"aa\")}")
    console.print("${\"abc\".count(\"x\")}")
    console.print("${\"abc\".count(\"\")}")
    console.print("${\"aéaéa\".count(\"éa\")}")
    console.print("witchy".char_at(0) ?? "?")
    console.print("witchy".char_at(5) ?? "?")
    console.print("[" + ("witchy".char_at(10) ?? "") + "]")
    console.print("[" + ("".char_at(0) ?? "") + "]")
    console.print("[" + "  hello  ".trim() + "]")
    console.print("[" + "  hi".trim_start() + "]")
    console.print("[" + "bye  ".trim_end() + "]")
    console.print("[" + "\t\n x \r\n".trim() + "]")
    console.print("[" + "nospace".trim() + "]")
"#;
        let sources = [("string", crate::bundled_module("string").unwrap()), ("main", client)];
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(compiled, vec![
            "hel", "[hi]", "[]", "llo", "[]", "ca", "é", "olleh", "[]", "éfac",
            "a/b.c", "heLlo", "xyz", "name", "witchy", "no-sep-here", "[]", "a", "b=c",
            "4", "the", "quick", "brown", "fox", "4", "c", "a", "f", "é", "0",
            "00042", "42...", "hello", "-=-=-ab", "**café", "café**",
            "lang", "witchy.lang", "main", "main.rs", "", "mile", "héllo", "naïve",
            "210", "true", "false", "3", "2", "2", "0", "0", "2",
            "w", "y", "[]", "[]", "[hello]", "[hi]", "[bye]", "[x]", "[nospace]",
        ]);
    }

    // is_empty/count and char_at cases are part of the transformations corpus above.

    // std/http get_url: parse a URL string and GET it (loopback). Interpreter-only.
    // std/string trimming: trim/trim_start/trim_end over assorted whitespace.
    // Pure, so both backends agree.
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

    /// std/url: malformed URLs return `Err` identically on both backends rather
    /// than accepting a blank scheme/host (BUG-187), swallowing a query into the
    /// host (BUG-249), or trapping on an oversized port (BUG-197).
    #[test]
    fn url_parse_rejects_malformed_on_both_backends() {
        let src = "import url\n\
                   fn show(label: String, s: String, console: Console):\n\
                   \x20   match url.parse(s):\n\
                   \x20       Ok(u) -> console.print(label + \": \" + url.scheme(u) + \"|\" + url.host(u) + \"|${url.port(u)}|\" + url.request_target(u))\n\
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
            "query: https|example.com|443|/?x=1",
            "big_port: ERR",
            "bad_port: ERR",
            "ok: https|example.com|443|/a/b",
        ];
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
        assert_eq!(wasm_run(src), expected, "wasm");
    }
