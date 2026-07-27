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
