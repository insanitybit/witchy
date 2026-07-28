use super::*;
use crate::{interpreter, typeck};

    /// (RFC-0054) `?` converts typed errors through `From`, so libraries can
    /// expose matchable enum errors without collapsing every layer to `String`.
    #[test]
    fn rfc0054_try_converts_errors_through_from_backends_agree() {
        let src = "import show\nimport error\nimport convert\n\ntype ParseError:\n    Bad(String)\n\nimpl Show for ParseError:\n    fn show(self) -> String:\n        match self:\n            Bad(s) -> \"parse:\" + s\n\nimpl Error for ParseError\n\ntype AppError:\n    Wrapped(String)\n\nimpl Show for AppError:\n    fn show(self) -> String:\n        match self:\n            Wrapped(s) -> \"app:\" + s\n\nimpl Error for AppError\n\nimpl From(ParseError) for AppError:\n    fn from(value: ParseError) -> Self:\n        match value:\n            Bad(s) -> Wrapped(\"wrapped \" + s)\n\nfn leaf() -> Result(Int, ParseError):\n    Err(Bad(\"nope\"))\n\nfn wrapper() -> Result(Int, AppError):\n    let x = leaf()?\n    Ok(x + 1)\n\nfn main(console: Console):\n    match wrapper():\n        Ok(n) -> console.print(\"${n}\")\n        Err(e) -> console.print(show.render(e))\n";
        let expected = ["app:wrapped nope"];
        assert_eq!(link_run(src), expected, "interp: From-converting ?");
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
    fn rfc0054_option_context_converts_through_string_from() {
        let src = "import show\nimport error\nimport convert\n\ntype AppError:\n    Message(String)\n\nimpl Show for AppError:\n    fn show(self) -> String:\n        match self:\n            Message(s) -> \"app:\" + s\n\nimpl Error for AppError\n\nimpl From(String) for AppError:\n    fn from(value: String) -> Self:\n        Message(value)\n\nfn find() -> Option(Int):\n    None\n\nfn wrapper() -> Result(Int, AppError):\n    let x = find()? \"missing value\"\n    Ok(x)\n\nfn main(console: Console):\n    match wrapper():\n        Ok(n) -> console.print(\"${n}\")\n        Err(e) -> console.print(show.render(e))\n";
        let expected = ["app:missing value"];
        assert_eq!(link_run(src), expected, "interp: Option ? context converts through From(String)");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: Option ? context converts through From(String)",
        );
    }

    #[test]
    fn rfc0054_plain_option_try_stays_option_scoped() {
        let src = "import show\nimport error\n\ntype AppError:\n    Missing\n\nimpl Show for AppError:\n    fn show(self) -> String:\n        \"missing\"\n\nimpl Error for AppError\n\nfn find() -> Option(Int):\n    None\n\nfn wrapper() -> Result(Int, AppError):\n    find()?\n";
        let err = typeck::check(&resolve_std_src(src)).expect_err("plain Option ? must not invent a typed error");
        assert!(err.to_string().contains("propagates from a `Option"), "{err}");
    }

    #[test]
    fn try_operator_result_backends_agree() {
        // `?` propagation on Result: the success path unwraps and continues, the
        // failure path short-circuits with the Err. Both backends must agree.
        let client = r#"
import result

fn parse_pos(n: Int) -> Result(Int, String):
    if (n > 0):
        Ok(n)
    else:
        Err("bad")

fn add_two(a: Int, b: Int) -> Result(Int, String):
    let x = (parse_pos(a))?
    let y = (parse_pos(b))?
    Ok((x + y))

fn main(console: Console):
    console.print("${result.unwrap_or(add_two(3, 4), 0)}")
    console.print("${result.unwrap_or(add_two(3, (0 - 1)), 0)}")
    console.print("${result.is_err(add_two((0 - 5), 2))}")
    console.print("${result.is_ok(add_two(10, 20))}")
"#;
        let sources = [("result", crate::bundled_module("result").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "`?` on Result diverged between backends");
    }

    #[test]
    fn try_operator_with_message_backends_agree() {
        // `e ? "msg"` adds context and is generic over the operand: an `Option`'s
        // `None` becomes `Err(msg)`; a `Result`'s `Err(e)` becomes `Err("msg: e")`.
        // Both backends must agree (the message form works wherever bare `?` does).
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
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "`? \"msg\"` diverged between backends");
        assert!(
            interpreted.iter().any(|l| l.contains("missing value")),
            "Option `None` must become `Err(msg)`: {interpreted:?}"
        );
        assert!(
            interpreted.iter().any(|l| l.contains("while computing: boom")),
            "Result `Err(e)` must become `Err(\"msg: e\")`: {interpreted:?}"
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
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "`?` on Option diverged between backends");
    }

    #[test]
    fn unwrap_or_else_backends_agree() {
        // Lazy defaults via a zero-arg closure, for both Option and Result.
        let opt = r#"
import option

fn main(console: Console):
    console.print("${option.unwrap_or_else(Some(5), fn(): 0)}")
    let fallback = 99
    console.print("${option.unwrap_or_else(option.filter(Some(3), fn(n: Int): (n > 10)), fn(): fallback)}")
"#;
        let osrc = [("option", crate::bundled_module("option").unwrap()), ("main", opt)];
        assert_eq!(
            interpreter::run_program(&osrc, "main").expect("interp"),
            run_linked_on_wasm(&osrc, "main")
        );
        assert_eq!(run_linked_on_wasm(&osrc, "main"), vec!["5", "99"]);

        let res = r#"
import result

fn checked(n: Int) -> Result(Int, String):
    if (n > 0):
        Ok(n)
    else:
        Err("bad")

fn main(console: Console):
    console.print("${result.unwrap_or_else(checked(7), fn(): 0)}")
    console.print("${result.unwrap_or_else(checked((0 - 1)), fn(): 42)}")
"#;
        let rsrc = [("result", crate::bundled_module("result").unwrap()), ("main", res)];
        assert_eq!(
            interpreter::run_program(&rsrc, "main").expect("interp"),
            run_linked_on_wasm(&rsrc, "main")
        );
        assert_eq!(run_linked_on_wasm(&rsrc, "main"), vec!["7", "42"]);
    }

    #[test]
    fn result_and_option_all_backends_agree() {
        // `all` sequences a list of Results/Options: Ok/Some of the collected
        // values, or the first failure (Err / None).
        let client = r#"
import result
import option
import list
fn nums(r: Result(List(Int), String)) -> String:
    match r:
        Ok(xs) -> list.join(list.map(xs, fn(n: Int): "${n}"), ",")
        Err(e) -> "err:" + e
fn onums(o: Option(List(Int))) -> String:
    match o:
        Some(xs) -> list.join(list.map(xs, fn(n: Int): "${n}"), ",")
        None -> "none"
fn main(console: Console):
    console.print(nums(result.all([Ok(1), Ok(2), Ok(3)])))
    console.print(nums(result.all([Ok(1), Err("bad"), Ok(3)])))
    console.print(nums(result.all([])))
    console.print(onums(option.all([Some(1), Some(2)])))
    console.print(onums(option.all([Some(1), None, Some(3)])))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("result", crate::bundled_module("result").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "result/option.all diverged");
        assert_eq!(compiled, vec!["1,2,3", "err:bad", "", "1,2", "none"]);
    }

    #[test]
    fn try_operator_runs_on_wasm() {
        // `?` compiles: success unwraps, error early-returns. compute(3,4)=Ok(7),
        // compute(0,9)=Err(99); 7*100 + 99 = 799.
        let src = r#"
fn checked(n: Int) -> Result(Int, Int):
    match n:
        0 -> Err(99)
        _ -> Ok(n)

fn compute(a: Int, b: Int) -> Result(Int, Int):
    let x = (checked(a))?
    let y = (checked(b))?
    Ok((x + y))

fn main() -> Int:
    let ok = match compute(3, 4):
        Ok(v) -> v
        Err(e) -> e
    let bad = match compute(0, 9):
        Ok(v) -> v
        Err(e) -> e
    ((ok * 100) + bad)
"#;
        assert_eq!(run_on_wasm(src), vec!["799"]);
    }
