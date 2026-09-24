use super::*;
use crate::{interpreter, parser, typeck};

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

