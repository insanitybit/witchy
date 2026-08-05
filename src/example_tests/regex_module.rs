use super::*;
use crate::{codegen, interpreter, typeck};

    /// The `std/regex` toolkit — greedy quantifiers, escapes (`\d`/`\w`/`\s` and
    /// literal metacharacters), character classes with ranges and negation, and
    /// the span-based API (`find`/`find_all`/`extract`/`replace_all`/`split`) —
    /// agrees on both backends, including the `Option((Int, Int))` span payload.
    #[test]
    fn regex_module_toolkit_agrees_on_both_backends() {
        let src = "import regex\n\nfn main(console: Console):\n    console.print(\"${regex.matches(\"h.llo\", \"say hello\")}\")\n    console.print(\"${regex.matches(\"^\\\\d+$\", \"12345\")}\")\n    console.print(\"${regex.matches(\"^\\\\d+$\", \"12a45\")}\")\n    console.print(\"${regex.extract(\"\\\\d+\", \"a1b22c333\")}\")\n    console.print(regex.replace_all(\"\\\\s+\", \"too   many    spaces\", \" \"))\n    console.print(\"${regex.split(\",\\\\s*\", \"a, b,c\")}\")\n    console.print(\"${regex.matches(\"[a-f]+\", \"deadbeef\")}\")\n    console.print(\"${regex.matches(\"^[^0-9]+$\", \"abc\")}\")\n    console.print(\"${regex.find(\"a+\", \"caat\")}\")\n    console.print(\"${regex.matches(\"\\\\w+@\\\\w+\\\\.\\\\w+\", \"mail me: a_b@example.com\")}\")\n    console.print(regex.replace_all(\"[0-9]+\", \"r2d2\", \"#\"))\n";
        let want: Vec<String> = [
            "true",
            "true",
            "false",
            "[1, 22, 333]",
            "too many spaces",
            "[a, b, c]",
            "true",
            "true",
            "Some((1, 3))",
            "true",
            "r#d#",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// (BUG-186/RFC-0044) An invalid regex pattern is a loud error, not the same
    /// result as a valid regex with no matches. That keeps the module docs, native
    /// helper, and compiled host import on one contract.
    #[test]
    fn regex_invalid_pattern_is_loud_on_both_backends() {
        let src = "import regex\n\nfn main(console: Console):\n    console.print(\"${regex.matches(\"[\", \"x\")}\")\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let interp_err = interpreter::run_module(linked.clone(), ".", Vec::new())
            .expect_err("interpreter must reject invalid regex syntax")
            .to_string();
        assert!(interp_err.contains("invalid regex pattern `[`"), "{interp_err}");

        let bytes = codegen::compile_module_binary(&linked)

            .expect_lowered("the binary path lowers regex");
        let wasm_err = crate::run_wasm_bytes(&bytes)
            .expect_err("WASM must reject invalid regex syntax")
            .to_string();
        assert!(wasm_err.contains("invalid regex pattern `[`"), "{wasm_err}");
    }

    /// Alternation `a|b` and grouping `(...)` — which the old hand-rolled engine
    /// silently failed to match — now work (the `regex` crate), identically on
    /// both backends, including grouped extract.
    #[test]
    fn regex_alternation_and_groups_agree_on_both_backends() {
        let src = "import regex\n\nfn main(console: Console):\n    console.print(\"${regex.matches(\"cat|dog\", \"I have a dog\")}\")\n    console.print(\"${regex.matches(\"(cat|dog)s?\", \"cats\")}\")\n    console.print(\"${regex.extract(\"(foo|bar)\", \"foo bar baz\")}\")\n    console.print(regex.replace_all(\"(a|b)+\", \"abab x\", \"Z\"))\n    console.print(\"${regex.find(\"(cat|dog)\", \"a dog\")}\")\n";
        let want: Vec<String> = ["true", "true", "[foo, bar]", "Z x", "Some((2, 5))"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }
