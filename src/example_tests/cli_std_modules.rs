use crate::{interpreter, parser};

    /// `main -> Int` sets the process exit code (C/Go/Rust convention) and is
    /// *not* printed; `main` returning Nil exits 0 and shows its `print` output.
    #[test]
    fn main_int_return_is_the_process_exit_code() {
        let run = |src: &str| {
            let m = parser::parse_module(src).expect("parse");
            let l = crate::pipeline::link(vec![("main".into(), m)], "main").expect("link");
            interpreter::run_module_exit(l, ".", Vec::new(), Vec::new(), None).expect("run")
        };
        let (out, code) = run("fn main() -> Int:\n    7\n");
        assert!(out.is_empty(), "an Int return must not be printed, got {out:?}");
        assert_eq!(code, 7);
        let (out, code) = run("fn main(console: Console):\n    console.print(\"hi\")\n");
        assert_eq!(out, vec!["hi"]);
        assert_eq!(code, 0);
    }

    /// `import list` resolves to the bundled standard library (no local file),
    /// links, type-checks, and runs end to end through the CLI.
    #[test]
    fn std_library_resolves_and_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/std_demo/src/std_demo.witchy", Vec::new()).unwrap(),
            vec!["30", "3"]
        );
    }

    /// Sorting with a comparator closure, end to end through the bundled std.
    #[test]
    fn sort_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/sort/src/sort.witchy", Vec::new()).unwrap(),
            vec!["1,1,3,4,5", "5,4,3,1,1"]
        );
    }

    /// The bundled `math` module resolves and computes via the CLI.
    #[test]
    fn math_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/math_demo/src/math_demo.witchy", Vec::new()).unwrap(),
            vec!["7", "5", "10", "1024", "12"]
        );
    }

    /// Float math: the `sqrt` builtin and the `math` module's Float helpers.
    #[test]
    fn floats_run_via_cli() {
        assert_eq!(
            crate::execute_file("examples/floats/src/floats.witchy", Vec::new()).unwrap(),
            vec!["4.0", "3.5", "5.0", "1.0"]
        );
    }

    /// The list module's search/slice helpers via the CLI.
    #[test]
    fn list_more_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/list_more/src/list_more.witchy", Vec::new()).unwrap(),
            vec!["true", "3", "-1", "20", "30"]
        );
    }

    /// The list-combinator pipeline example runs via the CLI (interpreter); a
    /// companion compiled test (`list_pipeline_example_runs_on_wasm`) asserts the
    /// same output through the WASM backend.
    #[test]
    fn list_pipeline_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/list_pipeline/src/list_pipeline.witchy", Vec::new()).unwrap(),
            vec!["233", "2 8", "735"]
        );
    }

    /// `zip`/`enumerate` and tuple destructuring in a loop, via the CLI.
    #[test]
    fn zip_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/zip/src/zip.witchy", Vec::new()).unwrap(),
            vec!["0:alice 1:bob 2:carol", "alice=30 bob=25 carol=40"]
        );
    }

    /// `any`/`all` predicate combinators via the CLI.
    #[test]
    fn predicates_run_via_cli() {
        assert_eq!(
            crate::execute_file("examples/predicates/src/predicates.witchy", Vec::new()).unwrap(),
            vec!["true", "true", "false", "false"]
        );
    }

    /// `all` is vacuously true on the empty list; `any` is false.
    #[test]
    fn any_all_empty_list_edge_cases() {
        let client = r#"
import list

fn main(console: Console):
    let empty = list.filter([1], fn(n: Int): (n > 100))
    console.print("${list.all(empty, fn(n: Int): (n > 0))}")
    console.print("${list.any(empty, fn(n: Int): (n > 0))}")
"#;
        let out = interpreter::run_program(
            &[("list", crate::bundled_module("list").unwrap()), ("main", client)],
            "main",
        )
        .expect("predicates program runs");
        assert_eq!(out, vec!["true", "false"]);
    }

    /// `zip` is generic and stops at the shorter list.
    #[test]
    fn zip_is_generic_and_truncates() {
        let client = r#"
import list

fn main(console: Console):
    let ps = list.zip([1, 2, 3], ["a", "b"])
    console.print("${list.length(ps)}")
    let first = list.at(ps, 0)
    let (n, s) = first
    console.print(("${n}" + s))
"#;
        let out = interpreter::run_program(
            &[("list", crate::bundled_module("list").unwrap()), ("main", client)],
            "main",
        )
        .expect("zip program runs");
        assert_eq!(out, vec!["2", "1a"]);
    }

    /// `contains`/`index_of` are generic — they work on Strings too (by value).
    #[test]
    fn list_contains_is_generic_over_element_type() {
        let client = r#"
import list

fn main(console: Console):
    let words = ["a", "bb", "ccc"]
    console.print("${list.contains(words, "bb")}")
    console.print("${list.index_of(words, "ccc") ?? -1}")
"#;
        let out = interpreter::run_program(
            &[("list", crate::bundled_module("list").unwrap()), ("main", client)],
            "main",
        )
        .expect("list program runs");
        assert_eq!(out, vec!["true", "2"]);
    }

    /// The bundled `option` module (type + helpers) resolves via the CLI.
    #[test]
    fn option_module_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/option_std/src/option_std.witchy", Vec::new()).unwrap(),
            vec!["10", "-1"]
        );
    }

    /// The bundled `result` module supplies the type `?` recognizes, plus
    /// helpers, when linked against a client.
    #[test]
    fn result_module_links_with_try_and_helpers() {
        let client = r#"
import result

fn checked_div(a: Int, b: Int) -> Result(Int, String):
    match b:
        0 -> Err("zero")
        _ -> Ok((a / b))

fn compute(x: Int, y: Int) -> Result(Int, String):
    let q = (checked_div(x, y))?
    Ok((q + 1))

fn main(console: Console):
    console.print("${result.unwrap_or(compute(10, 2), (0 - 1))}")
    console.print("${result.unwrap_or(compute(10, 0), (0 - 1))}")
    console.print("${result.is_ok(compute(10, 0))}")
"#;
        let out = interpreter::run_program(
            &[("result", crate::bundled_module("result").unwrap()), ("main", client)],
            "main",
        )
        .expect("result module program runs");
        assert_eq!(out, vec!["6", "-1", "false"]);
    }

    /// String builtins + the bundled `list`/`string` modules end to end.
    #[test]
    fn text_processing_runs_via_cli() {
        assert_eq!(
            crate::execute_file("examples/text/src/text.witchy", Vec::new()).unwrap(),
            vec!["ALICE | BOB | CAROL", "===", "alice,***,carol"]
        );
    }

    /// The bundled `list` module type-checks and links against a client program.
    #[test]
    fn bundled_list_module_links() {
        let client = r#"
import list

fn main(console: Console):
    let xs = list.map(list.range(4), fn(n: Int): (n + 1))
    console.print("${list.fold(xs, 0, fn(a: Int, b: Int): (a + b))}")
"#;
        let out = interpreter::run_program(
            &[("list", crate::bundled_module("list").unwrap()), ("main", client)],
            "main",
        )
        .expect("std list program runs");
        assert_eq!(out, vec!["10"]); // (1+2+3+4)
    }
