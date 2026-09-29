use super::*;
use crate::typeck;

    /// (RFC-0054) `?` converts typed errors through `From`, so libraries can
    /// expose matchable enum errors without collapsing every layer to `String`.
    #[test]
    fn rfc0054_try_converts_errors_through_from_backends_agree() {
        let src = "import show\nimport error\nimport convert\n\ntype ParseError:\n    Bad(String)\n\nimpl Show for ParseError:\n    fn show(self) -> String:\n        match self:\n            Bad(s) -> \"parse:\" + s\n\nimpl Error for ParseError\n\ntype AppError:\n    Wrapped(String)\n\nimpl Show for AppError:\n    fn show(self) -> String:\n        match self:\n            Wrapped(s) -> \"app:\" + s\n\nimpl Error for AppError\n\nimpl From(ParseError) for AppError:\n    fn from(value: ParseError) -> Self:\n        match value:\n            Bad(s) -> Wrapped(\"wrapped \" + s)\n\nfn leaf() -> Result(Int, ParseError):\n    Err(Bad(\"nope\"))\n\nfn wrapper() -> Result(Int, AppError):\n    let x = leaf()?\n    Ok(x + 1)\n\nfn main(console: Console):\n    match wrapper():\n        Ok(n) -> console.print(\"${n}\")\n        Err(e) -> console.print(show.render(e))\n";
        let expected = ["app:wrapped nope"];
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: From-converting ?",
        );
    }

    #[test]
    fn rfc0054_try_rejects_missing_from_error_conversion() {
        let src = "import show\nimport error\n\ntype LeafError:\n    Leaf\n\nimpl Show for LeafError:\n    fn show(self) -> String:\n        \"leaf\"\n\nimpl Error for LeafError\n\ntype AppError:\n    App\n\nimpl Show for AppError:\n    fn show(self) -> String:\n        \"app\"\n\nimpl Error for AppError\n\nfn leaf() -> Result(Int, LeafError):\n    Err(Leaf)\n\nfn wrapper() -> Result(Int, AppError):\n    leaf()?\n";
        let err = typeck::check(&resolve_std_src(src)).expect_err("missing From conversion must reject");
        assert!(err.to_string().contains("no `From("), "{err}");
    }

    #[test]
    fn try_operator_with_message_backends_agree() {
        // `e ? "msg"` adds context and is generic over the operand: an `Option`'s
        // `None` becomes `Err(msg)`; a `Result`'s `Err(e)` becomes `Err("msg: e")`.
        // The message form works wherever bare `?` does.
        let client = r#"
import option
import result

fn need(o: Option(Int)) -> Result(Int, String):
    let x = o ? "missing value"
    Ok(x)

fn rewrap(r: Result(Int, String)) -> Result(Int, String):
    let x = r ? "while computing"
    Ok(x)

fn main(console: Console):
    console.print("${need(Some(5))}")
    console.print("${need(None)}")
    console.print("${rewrap(Ok(9))}")
    console.print("${rewrap(Err("boom"))}")
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("result", crate::bundled_module("result").unwrap()),
            ("main", client),
        ];
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(compiled.len(), 4, "one result per call: {compiled:?}");
        assert!(compiled[0].contains('5'), "Some(5) must succeed: {compiled:?}");
        assert!(compiled[2].contains('9'), "Ok(9) must succeed: {compiled:?}");
        assert!(
            compiled[1].contains("missing value"),
            "Option `None` must become `Err(msg)`: {compiled:?}"
        );
        assert!(
            compiled[3].contains("while computing: boom"),
            "Result `Err(e)` must become `Err(\"msg: e\")`: {compiled:?}"
        );
    }

    #[test]
    fn try_operator_option_backends_agree() {
        // `?` propagation on Option: short-circuit on None, unwrap on Some.
        let client = r#"
import option

fn first_even(a: Int, b: Int) -> Option(Int):
    let x = (pick_even(a))?
    let y = (pick_even(b))?
    Some((x + y))

fn pick_even(n: Int) -> Option(Int):
    if ((n % 2) == 0):
        Some(n)
    else:
        None

fn main(console: Console):
    console.print("${option.unwrap_or(first_even(4, 6), 0)}")
    console.print("${option.unwrap_or(first_even(4, 7), 0)}")
    console.print("${option.is_none(first_even(3, 8))}")
"#;
        let sources = [("option", crate::bundled_module("option").unwrap()), ("main", client)];
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(compiled, ["10", "0", "true"], "Option `?` propagation");
    }
