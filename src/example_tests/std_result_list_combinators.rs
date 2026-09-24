use super::*;
use crate::{interpreter};

    // windows: sliding sublists of length n (step 1), empty when n exceeds the
    // list or n < 1. Complements chunks. Iterating List(List(Int)) too.
    #[test]
    fn std_list_windows_backends_agree() {
        let client = r#"
import list

fn main(console: Console):
    let ws = list.windows([1, 2, 3, 4], 2)
    console.print("${list.length(ws)}")
    for w in ws:
        console.print("${list.sum(w)}")
    console.print("${list.length(list.windows([1, 2], 5))}")
    console.print("${list.length(list.windows([1, 2, 3], 0))}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "windows diverged");
        assert_eq!(compiled, vec!["3", "3", "5", "7", "0", "0"]);
    }

    // split_at splits a list into (first n, the rest); n is clamped at both
    // ends. The list analogue of string.split_once. Both backends agree.
    #[test]
    fn std_list_split_at_backends_agree() {
        let client = r#"
import list

fn main(console: Console):
    let (a, b) = list.split_at([1, 2, 3, 4, 5], 2)
    console.print("${list.sum(a)}")
    console.print("${list.sum(b)}")
    let (c, d) = list.split_at([1, 2], 5)
    console.print("${list.sum(c)}")
    console.print("${list.length(d)}")
    let (e, f) = list.split_at([1, 2, 3], 0)
    console.print("${list.length(e)}")
    console.print("${list.sum(f)}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "split_at diverged");
        assert_eq!(compiled, vec!["3", "12", "3", "0", "0", "6"]);
    }

