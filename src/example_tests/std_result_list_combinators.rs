use super::*;
use crate::{interpreter};

    // The composable, total lookups: list.head/last/get/find return Option
    // (None instead of an out-of-bounds trap). `list` imports `option`, and the
    // caller provides only `main` — the linker auto-resolves both std modules.
    // More total Option-returning list functions: min/max (None for the empty
    // list) and position (the Option counterpart to index_of's -1 sentinel).
    // result -> option conversions (result imports option): `ok` keeps the Ok
    // value as Some and drops an Err to None; `err` does the reverse. Caller
    // provides only `main`; the linker resolves result and option.
    // option -> result conversions (option imports result, completing the
    // Option<->Result pair; the linker flattens the cyclic import). ok_or maps
    // Some to Ok and None to Err(err); ok_or_else computes the error lazily.
    #[test]
    fn std_option_to_result_backends_agree() {
        let client = r#"
import option
import result

fn main(console: Console):
    console.print("${result.unwrap_or(option.ok_or(Some(5), "none"), 0)}")
    console.print("${result.is_err(option.ok_or(None, "none"))}")
    console.print("${result.unwrap_or(option.ok_or_else(Some(9), fn(): "none"), 0)}")
    console.print("${result.is_err(option.ok_or_else(None, fn(): "none"))}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "option->result diverged");
        assert_eq!(compiled, vec!["5", "true", "9", "true"]);
    }

    // result.flatten collapses Result(Result(a, e), e) one level (Ok(Ok(v)) ->
    // Ok(v); Ok(Err) and Err -> Err), mirroring option.flatten. Both backends agree.
    #[test]
    fn std_result_flatten_backends_agree() {
        let client = r#"
import result

fn nested(n: Int) -> Result(Result(Int, String), String):
    if (n > 0):
        Ok(Ok(n))
    else:
        Ok(Err("inner"))

fn main(console: Console):
    console.print("${result.unwrap_or(result.flatten(nested(5)), 0)}")
    console.print("${result.is_err(result.flatten(nested(0)))}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "result flatten diverged");
        assert_eq!(compiled, vec!["5", "true"]);
    }

    #[test]
    fn std_result_to_option_backends_agree() {
        let client = r#"
import result
import option

fn check(n: Int) -> Result(Int, String):
    if (n > 0):
        Ok(n)
    else:
        Err("bad")

fn main(console: Console):
    console.print("${option.unwrap_or(result.ok(check(5)), 0)}")
    console.print("${option.is_none(result.ok(check((0 - 1))))}")
    console.print("${option.is_none(result.err(check(5)))}")
    console.print("${option.unwrap_or(result.err(check((0 - 1))), "").length()}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "result->option diverged");
        assert_eq!(compiled, vec!["5", "true", "true", "3"]);
    }

    // find_map searches and transforms in one pass: the first non-None result
    // of f, or None. Here it returns half of the first even number.
    // reduce folds with the first element as the seed (Option-returning, None
    // for empty) — here used as max and sum without an explicit initial value.
    #[test]
    fn std_list_reduce_backends_agree() {
        let client = r#"
import list
import option

fn main(console: Console):
    let mx = list.reduce([3, 1, 4, 1, 5], fn(a: Int, b: Int): if (a > b): a else: b)
    console.print("${option.unwrap_or(mx, 0)}")
    console.print("${option.is_none(list.reduce([], fn(a: Int, b: Int): (a + b)))}")
    let sum = list.reduce([10, 20, 30], fn(a: Int, b: Int): (a + b))
    console.print("${option.unwrap_or(sum, 0)}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "reduce diverged");
        assert_eq!(compiled, vec!["5", "true", "60"]);
    }

    #[test]
    fn std_list_find_map_backends_agree() {
        let client = r#"
import list
import option

fn main(console: Console):
    let r = list.find_map([3, 5, 8, 10], fn(x: Int): if ((x % 2) == 0): Some((x / 2)) else: None)
    console.print("${option.unwrap_or(r, (0 - 1))}")
    let none = list.find_map([1, 3, 5], fn(x: Int): if (x > 100): Some(x) else: None)
    console.print("${option.is_none(none)}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "find_map diverged");
        assert_eq!(compiled, vec!["4", "true"]);
    }

    #[test]
    fn std_list_option_lookups_backends_agree() {
        let client = r#"
import list
import option

fn main(console: Console):
    console.print("${option.unwrap_or(list.head([10, 20]), 0)}")
    console.print("${option.unwrap_or(list.head([]), (0 - 1))}")
    console.print("${option.unwrap_or(list.last([10, 20]), 0)}")
    console.print("${option.unwrap_or(list.get([10, 20, 30], 1), 0)}")
    console.print("${option.unwrap_or(list.get([10], 5), (0 - 1))}")
    console.print("${option.unwrap_or(list.find([1, 3, 4], fn(n: Int): ((n % 2) == 0)), (0 - 1))}")
    console.print("${option.is_none(list.find([1, 3, 5], fn(n: Int): ((n % 2) == 0)))}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "list option lookups diverged");
        assert_eq!(compiled, vec!["10", "-1", "20", "20", "-1", "4", "true"]);
    }

    #[test]
    fn std_list_head_last_find_or_backends_agree() {
        // Total accessors: head_or/last_or return a default for the empty list
        // (never indexing out of bounds), and find_or returns the first match or
        // a default. Both backends agree.
        let client = r#"
import list

fn main(console: Console):
    console.print("${list.head_or([10, 20, 30], 0)}")
    console.print("${list.head_or([], (0 - 1))}")
    console.print("${list.last_or([10, 20, 30], 0)}")
    console.print("${list.last_or([], (0 - 1))}")
    console.print("${list.find_or([1, 3, 4, 7], fn(n: Int): ((n % 2) == 0), (0 - 1))}")
    console.print("${list.find_or([1, 3, 5], fn(n: Int): ((n % 2) == 0), (0 - 1))}")
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "head_or/last_or/find_or diverged");
        assert_eq!(compiled, vec!["10", "-1", "30", "-1", "4", "-1"]);
    }

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

    #[test]
    fn std_list_chunks_tail_init_backends_agree() {
        // chunks groups into fixed-size sublists (last may be short), tail drops
        // the first element, init drops the last — all total (empty stays empty).
        // Iterating List(List(Int)) also exercises nested lists across backends.
        let client = r#"
import list

fn main(console: Console):
    let cs = list.chunks([1, 2, 3, 4, 5], 2)
    console.print("${list.length(cs)}")
    for c in cs:
        console.print("${list.sum(c)}")
    console.print("${list.sum(list.tail([1, 2, 3]))}")
    console.print("${list.sum(list.drop_last([1, 2, 3]))}")
    console.print("${list.length(list.tail([]))}")
    console.print("${list.length(list.drop_last([]))}")
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "chunks/tail/init diverged");
        assert_eq!(compiled, vec!["3", "3", "7", "5", "5", "3", "0", "0"]);
    }

    // sum_by totals a projection of each element (0 for empty) — including a
    // record field via a record-typed lambda parameter.
    #[test]
    fn std_list_sum_by_backends_agree() {
        let client = r#"
import list

type Item:
    price: Int
    qty: Int

fn main(console: Console):
    let cart = [Item(50, 3), Item(200, 1), Item(150, 2)]
    console.print("${list.sum_by(cart, fn(it: Item): ((it).price * (it).qty))}")
    console.print("${list.sum_by([1, 2, 3, 4], fn(n: Int): (n * n))}")
    console.print("${list.sum_by([], fn(n: Int): n)}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "sum_by diverged");
        assert_eq!(compiled, vec!["650", "30", "0"]);
    }

    // Option boolean combinators: is_some_and (predicate on a present value),
    // and (value-forgetting conjunction), xor (exactly one Some), and the
    // Eq-bounded contains. Method form; both backends agree.
    #[test]
    fn std_option_boolean_combinators_backends_agree() {
        let client = r#"
import option

fn main(console: Console):
    console.print("${Some(4).is_some_and(fn(x): (x > 2))} ${Some(1).is_some_and(fn(x): (x > 2))} ${None.is_some_and(fn(x): (x > 2))}")
    console.print("${Some(1).and(Some(2)).unwrap_or(0)} ${None.and(Some(2)).unwrap_or(0)}")
    console.print("${Some(1).xor(None).unwrap_or(9)} ${Some(1).xor(Some(2)).unwrap_or(9)} ${None.xor(Some(2)).unwrap_or(9)}")
    console.print("${Some(5).contains(5)} ${Some(5).contains(6)} ${None.contains(5)}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "option boolean combinators diverged");
        assert_eq!(
            compiled,
            vec!["true false false", "2 0", "1 9 2", "true false false"]
        );
    }

    // Result boolean combinators: is_ok_and / is_err_and (predicate on the
    // matching arm) and the Eq-bounded contains / contains_err. Method form;
    // both backends agree.
    #[test]
    fn std_result_boolean_combinators_backends_agree() {
        let client = r#"
import result

fn main(console: Console):
    let r: Result(Int, String) = Ok(10)
    let e: Result(Int, String) = Err("bad")
    console.print("${r.is_ok_and(fn(x): (x > 5))} ${r.is_err_and(fn(x): true)}")
    console.print("${e.is_err_and(fn(x): (x == "bad"))} ${e.is_ok_and(fn(x): true)}")
    console.print("${r.contains(10)} ${r.contains(11)} ${e.contains(10)}")
    console.print("${e.contains_err("bad")} ${e.contains_err("other")} ${r.contains_err("bad")}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "result boolean combinators diverged");
        assert_eq!(
            compiled,
            vec![
                "true false",
                "true false",
                "true false false",
                "true false false"
            ]
        );
    }

    // Set.is_superset is the reverse of is_subset: this set contains every
    // member of the other. Both backends agree.
    #[test]
    fn std_set_is_superset_backends_agree() {
        let client = r#"
import set

fn main(console: Console):
    let a = set.from_list([1, 2, 3])
    let b = set.from_list([1, 2])
    console.print("${a.is_superset(b)} ${b.is_superset(a)}")
    console.print("${a.is_superset(a)} ${a.is_superset(set.from_list([]))}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "set is_superset diverged");
        assert_eq!(compiled, vec!["true false", "true true"]);
    }

    #[test]
    fn std_list_product_slice_scan_backends_agree() {
        // product (1 for empty), slice (clamped half-open range), and scan
        // (running fold collecting intermediates) all agree across backends.
        let client = r#"
import list

fn main(console: Console):
    console.print("${list.product([1, 2, 3, 4])}")
    console.print("${list.product([])}")
    let s = list.slice([10, 20, 30, 40, 50], 1, 4)
    for x in s:
        console.print("${x}")
    let running = list.scan([1, 2, 3], 0, fn(acc: Int, n: Int): (acc + n))
    for x in running:
        console.print("${x}")
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "product/slice/scan diverged");
        assert_eq!(compiled, vec!["24", "1", "20", "30", "40", "0", "1", "3", "6"]);
    }
