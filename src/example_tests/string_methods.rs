use super::*;
use crate::{interpreter, parser, typeck};

    /// RFC-0050 Part 1: ambient builtin types whose API home is a std module are
    /// method-capable through that owner. Bytes and Duration were the motivating
    /// holes in the old hardcoded UFCS allowlist.
    #[test]
    fn rfc0050_builtin_type_owners_backends_agree() {
        let src = "import bytes\nimport duration\n\nfn main(console: Console):\n    let b = bytes.from_string(\"hello\")\n    console.print(\"${b.length()} ${b.slice(1, 4).to_string()}\")\n    let d = duration.seconds(3661)\n    console.print(\"${d.to_seconds()} ${d.abs().to_seconds()}\")\n";
        let expected = ["5 ell", "3661 3661"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (BUG-508) `string.split_once_opt` / `rsplit_once_opt` preserve the
    /// missing-separator bit that the legacy tuple helpers erase.
    #[test]
    fn string_split_once_option_helpers_backends_agree() {
        let src = "\nfn show(console: Console, label: String, p: Option((String, String))):\n    match p:\n        Some(parts) ->\n            let (a, b) = parts\n            console.print(label + \"=Some(\" + a + \"|\" + b + \")\")\n        None -> console.print(label + \"=None\")\n\nfn main(console: Console):\n    show(console, \"missing\", \"host\".split_once_opt(\":\"))\n    show(console, \"present-empty-right\", \"host:\".split_once_opt(\":\"))\n    show(console, \"present-empty-left\", \":name\".split_once_opt(\":\"))\n    show(console, \"last\", \"a.b.c\".rsplit_once_opt(\".\"))\n    show(console, \"last-missing\", \"name\".rsplit_once_opt(\".\"))\n    let (a, b) = \"host\".split_once(\":\")\n    console.print(\"old-first=\" + a + \"|\" + b)\n    let (c, d) = \"name\".rsplit_once(\".\")\n    console.print(\"old-last=\" + c + \"|\" + d)\n";
        let expected = [
            "missing=None",
            "present-empty-right=Some(host|)",
            "present-empty-left=Some(|name)",
            "last=Some(a.b|c)",
            "last-missing=None",
            "old-first=host|",
            "old-last=|name",
        ];
        assert_eq!(link_run(src), expected, "interp: split_once_opt preserves absence");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: split_once_opt preserves absence",
        );
    }

    /// (BUG-231) Tail slices after a matched prefix are character-indexed. Code
    /// must derive the start from `char_count`, not `length`'s UTF-8 byte count,
    /// or non-ASCII keys skip past the value.
    #[test]
    fn string_tail_slices_use_character_counts_on_both_backends() {
        let src = "\nfn value_after_eq(kv: String, name: String) -> String:\n    if kv.starts_with(name + \"=\"):\n        kv.drop(name.char_count() + 1)\n    else:\n        \"\"\n\nfn main(console: Console):\n    console.print(value_after_eq(\"naïve=x\", \"naïve\"))\n    console.print(\"éclair\".drop(1))\n";
        let expected = ["x", "clair"];
        assert_eq!(link_run(src), expected, "interp: char-count tail slice");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: char-count tail slice",
        );
    }

    /// `string.join` is the module-symmetric inverse of `string.split`, while
    /// the existing `list.join`/`parts.join` spellings remain valid.
    #[test]
    fn string_join_alias_backends_agree() {
        let src = "\nfn main(console: Console):\n    let parts = \"a,b,c\".split(\",\")\n    console.print(parts.join(\"-\"))\n    console.print(parts.join(\"|\"))\n    console.print([].join(\",\"))\n";
        let expected = ["a-b-c", "a|b|c", ""];
        assert_eq!(link_run(src), expected, "interp: string.join");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: string.join",
        );
    }

    /// RFC-0050 Part 1: for ordinary module-scoped types, method ownership is
    /// derived from the canonical `module.Type` name, so package/user modules get
    /// receiver-first methods without being listed in the compiler.
    #[test]
    fn rfc0050_user_type_owner_methods_backends_agree() {
        let matrix = "type Matrix:\n    Matrix(Int)\n\npub fn value(m: Matrix) -> Int:\n    match m:\n        Matrix(n) -> n\n\npub fn shifted(m: Matrix, delta: Int) -> Matrix:\n    match m:\n        Matrix(n) -> Matrix(n + delta)\n\nfn secret(m: Matrix) -> Int:\n    99\n";
        let main = "import matrix\n\nfn main(console: Console):\n    let m = matrix.Matrix(40)\n    console.print(\"${m.value()} ${m.shifted(2).value()}\")\n";
        let sources = [("matrix", matrix), ("main", main)];
        let expected = vec!["40 42".to_string()];
        assert_eq!(interpreter::run_program(&sources, "main").expect("interp"), expected);
        assert_eq!(run_linked_on_wasm(&sources, "main"), expected, "wasm");

        let bad_main = "import matrix\n\nfn main(console: Console):\n    let m = matrix.Matrix(1)\n    console.print(\"${m.secret()}\")\n";
        let linked = crate::pipeline::link(
            vec![
                ("matrix".to_string(), parser::parse_module(matrix).expect("parse matrix")),
                ("main".to_string(), parser::parse_module(bad_main).expect("parse main")),
            ],
            "main",
        )
        .expect("link");
        let err = typeck::check(&linked).expect_err("private owner helper is not a method").message;
        assert!(err.contains("no method `secret`"), "got: {err}");
    }

    /// (RFC-0050) Option and Result expose their primary combinators as real
    /// methods, while the module functions remain available as first-class
    /// helpers.
    #[test]
    fn option_and_result_methods_work_on_both_backends() {
        let src = r#"import option
import result
import show

fn main(console: Console):
    let some = Some(2)
    let none: Option(Int) = None
    console.print("${some.is_some()}|${none.is_none()}")
    console.print("${some.unwrap_or(0)}|${none.unwrap_or_else(fn(): 5)}")
    console.print(show(some.map(fn(x: Int): x + 3)))
    console.print("${some.map_or(0, fn(x: Int): x * 10)}|${none.map_or(9, fn(x: Int): x * 10)}")
    console.print(show(some.and_then(fn(x: Int): Some(x * 2))))
    console.print(show(some.filter(fn(x: Int): x > 1)) + "|" + show(some.filter(fn(x: Int): x > 3)))
    console.print(show(none.or(Some(7))))
    console.print(show(none.or_else(fn(): Some(8))))
    console.print(show(some.ok_or("missing")) + "|" + show(none.ok_or("missing")))
    let nested_opt: Option(Option(Int)) = Some(Some(4))
    console.print(show(nested_opt.flatten()))
    console.print(show(some.zip(Some("x"))))

    let ok: Result(Int, String) = Ok(4)
    let err: Result(Int, String) = Err("bad")
    console.print("${ok.is_ok()}|${err.is_err()}|${err.unwrap_err_or("none")}")
    console.print(show(ok.map_ok(fn(x: Int): x + 1)) + "|" + show(err.map_ok(fn(x: Int): x + 1)))
    console.print(show(err.map_err(fn(e: String): e + "!")))
    console.print("${ok.map_or(0, fn(x: Int): x * 2)}|${err.map_or(7, fn(x: Int): x * 2)}")
    console.print(show(ok.or(Ok(9))) + "|" + show(err.or(Ok(9))))
    console.print(show(err.or_else(fn(e: String): Ok(3))))
    console.print("${err.unwrap_or(12)}|${err.unwrap_or_else(fn(): 13)}")
    console.print(show(ok.ok()) + "|" + show(err.err()))
    let nested_res: Result(Result(Int, String), String) = Ok(Ok(11))
    console.print(show(nested_res.flatten()))
    console.print("${result.unwrap_or(option.ok_or(Some(6), "missing"), 0)}")
"#;
        let expected = [
            "true|true",
            "2|5",
            "Some(5)",
            "20|9",
            "Some(4)",
            "Some(2)|None",
            "Some(7)",
            "Some(8)",
            "Ok(2)|Err(missing)",
            "Some(4)",
            "Some((2, x))",
            "true|true|bad",
            "Ok(5)|Err(bad)",
            "Err(bad!)",
            "8|7",
            "Ok(4)|Ok(9)",
            "Ok(3)",
            "12|13",
            "Some(4)|Some(bad)",
            "Ok(11)",
            "6",
        ];
        assert_eq!(link_run(src), expected, "interp: option/result methods");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: option/result methods",
        );
    }

    #[test]
    fn trim_backends_agree() {
        // trim now compiles: leading/trailing ASCII whitespace (spaces, tabs,
        // newlines, CRs) is stripped; an all-whitespace string trims to "".
        let src = r#"
fn main(console: Console):
    console.print("  hello  ".trim())
    console.print("\t\nfoo\r\n".trim())
    console.print("nospaces".trim())
    console.print("   ".trim())
    console.print("${"  a b  ".trim().length()}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["hello", "foo", "nospaces", "", "3"]);
    }
