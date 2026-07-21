use super::*;
use crate::{interpreter};

    // max_by/min_by generalize min/max to any type via a comparator, returning
    // Option. The second comparator (`(0-a) < (0-b)`, i.e. larger magnitude is
    // "less") shows the result tracks the supplied ordering, not the natural one.
    // A variable bound to a record-typed constructor field in a match pattern
    // (`Circle(c)`) now resolves field access in the arm body (`c.x`). Codegen
    // previously rejected this; it's fixed for concrete (non-generic) field
    // types. Both backends agree.
    // Matching the Some of a function-returned Option(Record) binds the payload
    // to its record type, so `a.balance` resolves. Codegen learns the payload
    // record from the function's declared `-> Option(Account)` return.
    // Let-bound intermediates inherit derived types: `let o = lookup()` carries
    // the Option(Account) payload (so a later `match o { Some(a) -> a.balance }`
    // resolves), and `let xs = mk()` carries the List(P) element type (so
    // `for p in xs { p.x }` resolves). Both backends agree.
    #[test]
    fn let_bound_derived_types_backends_agree() {
        let client = r#"
import option

type Account:
    id: Int
    balance: Int

type P:
    x: Int
    y: Int

fn lookup(n: Int) -> Option(Account):
    if (n > 0):
        Some(Account(n, (n * 100)))
    else:
        None

fn mk() -> List(P):
    [P(1, 2), P(3, 4)]

fn main(console: Console):
    let o = lookup(7)
    match o:
        Some(a) -> console.print("${(a).balance}")
        None -> console.print("none")
    let xs = mk()
    for p in xs:
        console.print("${(p).x}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "let-bound derived types diverged");
        assert_eq!(compiled, vec!["700", "1", "3"]);
    }

    // The generic stdlib case: `list.find` etc. have shape `fn(List(a),..) ->
    // Option(a)`, so matching their result binds the payload to the list's
    // element record type. `acc.field` now resolves through a generic lookup.
    // Generic `fn(List(a),..) -> List(a)` results (filter/reverse/...) carry the
    // argument's element record type, so iterating them resolves field access:
    // `for p in list.filter(records, pred) { p.field }`.
    // map's result element type is the mapper's return type, so iterating a
    // `list.map(records, fn(r){ OtherRecord(..) })` resolves field access on the
    // mapped records (a different record type than the input).
    // End-to-end: records flow through the whole stdlib pipeline with correct
    // field resolution — fold over records, max_by/find returning Option(record)
    // (match payload reads fields), filter then iterate (loop var reads fields),
    // a helper function over a record, and first-class lambdas throughout.
    // The `?` operator unwrapping a Result(Record): `let acc = lookup(n)?` binds
    // acc to the payload record so `acc.balance` resolves, and an Err short-
    // circuits the enclosing Result-returning function. Both backends agree.
    #[test]
    fn try_operator_record_payload_backends_agree() {
        let client = r#"
import result

type Account:
    id: Int
    balance: Int

fn lookup(n: Int) -> Result(Account, String):
    if (n > 0):
        Ok(Account(n, (n * 100)))
    else:
        Err("bad")

fn process(n: Int) -> Result(Int, String):
    let acc = (lookup(n))?
    Ok(((acc).balance + 1))

fn main(console: Console):
    match process(5):
        Ok(v) -> console.print("${v}")
        Err(e) -> console.print(e)
    match process((0 - 1)):
        Ok(v) -> console.print("${v}")
        Err(e) -> console.print(e)
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "? with Result(Record) diverged");
        assert_eq!(compiled, vec!["501", "bad"]);
    }

    // Integration showcase: a recursive JSON-value renderer. Exercises a
    // recursive ADT (JArr holds List(Json)), every match arm form, recursion,
    // list.map with a *named function* argument (function-as-value), and
    // list.join — all composing. Both backends agree.
    #[test]
    fn json_renderer_integration_backends_agree() {
        let client = r#"
import list

type Json:
    JNull
    JBool(Bool)
    JNum(Int)
    JStr(String)
    JArr(List(Json))

fn render(j: Json) -> String:
    match j:
        JNull -> "null"
        JBool(b) -> if b: "true" else: "false"
        JNum(n) -> "${n}"
        JStr(s) -> (("\"" + s) + "\"")
        JArr(items) -> (("[" + list.join(list.map(items, render), ",")) + "]")

fn main(console: Console):
    let doc = JArr([JNum(1), JStr("hi"), JBool(true), JNull, JArr([JNum(2), JNum(3)])])
    console.print(render(doc))
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "json renderer diverged");
        assert_eq!(compiled, vec!["[1,\"hi\",true,null,[2,3]]"]);
    }

    #[test]
    fn order_processing_integration_backends_agree() {
        let client = r#"
import list
import option

type Item:
    name: String
    price: Int
    qty: Int

fn line_total(it: Item) -> Int:
    ((it).price * (it).qty)

fn main(console: Console):
    let cart = [Item("apple", 50, 3), Item("bread", 200, 1), Item("milk", 150, 2)]
    let total = list.fold(cart, 0, fn(acc: Int, it: Item): (acc + line_total(it)))
    console.print("${total}")
    match list.max_by(cart, fn(a: Item, b: Item): (line_total(a) < line_total(b))):
        Some(it) -> console.print((it).name)
        None -> console.print("none")
    let multi = list.filter(cart, fn(it: Item): ((it).qty > 1))
    for it in multi:
        console.print((it).name)
    match list.find(cart, fn(it: Item): ((it).name == "bread")):
        Some(it) -> console.print("${(it).price}")
        None -> console.print("0")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "order processing diverged");
        assert_eq!(compiled, vec!["650", "milk", "apple", "milk", "200"]);
    }

    #[test]
    fn iterate_map_result_records_backends_agree() {
        let client = r#"
import list

type Raw:
    a: Int
    b: Int

type Point:
    x: Int
    y: Int

fn main(console: Console):
    let raws = [Raw(1, 2), Raw(3, 4)]
    let pts = list.map(raws, fn(r: Raw): Point(((r).a + (r).b), ((r).a * (r).b)))
    for p in pts:
        console.print("${(p).x}")
    for p in list.map(raws, fn(r: Raw): Point((r).b, (r).a)):
        console.print("${(p).y}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "iterate map result diverged");
        assert_eq!(compiled, vec!["3", "7", "1", "3"]);
    }

    #[test]
    fn iterate_generic_list_result_records_backends_agree() {
        let client = r#"
import list

type P:
    x: Int
    y: Int

fn main(console: Console):
    let ps = [P(1, 10), P(2, 20), P(3, 30)]
    let evens = list.filter(ps, fn(p: P): (((p).x % 2) == 0))
    for p in evens:
        console.print("${(p).y}")
    var reversed = ps
    list.reverse(reversed)
    for p in reversed:
        console.print("${(p).x}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "iterate generic list result diverged");
        assert_eq!(compiled, vec!["20", "3", "2", "1"]);
    }

    #[test]
    fn match_generic_list_lookup_payload_backends_agree() {
        let client = r#"
import list
import option

type Account:
    id: Int
    balance: Int

fn main(console: Console):
    let accounts = [Account(1, 100), Account(2, 200), Account(3, 300)]
    match list.find(accounts, fn(a: Account): ((a).balance > 150)):
        Some(acc) -> console.print("${(acc).balance}")
        None -> console.print("none")
    match list.head(accounts):
        Some(acc) -> console.print("${(acc).id}")
        None -> console.print("none")
    match list.find(accounts, fn(a: Account): ((a).balance > 999)):
        Some(acc) -> console.print("${(acc).id}")
        None -> console.print("none")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "generic list lookup payload diverged");
        assert_eq!(compiled, vec!["200", "1", "none"]);
    }

    #[test]
    fn match_option_record_payload_backends_agree() {
        let client = r#"
import option

type Account:
    id: Int
    balance: Int

fn lookup(n: Int) -> Option(Account):
    if (n > 0):
        Some(Account(n, (n * 100)))
    else:
        None

fn main(console: Console):
    match lookup(5):
        Some(a) -> console.print("${(a).balance}")
        None -> console.print("none")
    match lookup((0 - 1)):
        Some(a) -> console.print("${(a).balance}")
        None -> console.print("none")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "Option(Record) match diverged");
        assert_eq!(compiled, vec!["500", "none"]);
    }

    // Nested constructor patterns destructure through a record: `Circle(Point(x,
    // y))` binds x and y from the inner Point in one pattern. Both backends agree.
    #[test]
    fn nested_constructor_pattern_backends_agree() {
        let src = r#"
type Point:
    x: Int
    y: Int

type Shape:
    Circle(Point)
    Origin

fn f(s: Shape) -> Int:
    match s:
        Circle(Point(x, y)) -> (x + y)
        Origin -> 0

fn main(console: Console):
    console.print("${f(Circle(Point(3, 4)))}")
    console.print("${f(Circle(Point(10, 1)))}")
    console.print("${f(Origin)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "nested constructor pattern diverged");
        assert_eq!(run_on_wasm(src), vec!["7", "11", "0"]);
    }

    #[test]
    fn match_binds_record_field_backends_agree() {
        let src = r#"
type Point:
    x: Int
    y: Int

type Shape:
    Circle(Point)
    Rect(Int, Int)

fn describe(s: Shape) -> Int:
    match s:
        Circle(c) -> ((c).x + (c).y)
        Rect(w, h) -> (w * h)

fn main(console: Console):
    console.print("${describe(Circle(Point(3, 4)))}")
    console.print("${describe(Rect(5, 6))}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "match record-field bind diverged");
        assert_eq!(run_on_wasm(src), vec!["7", "30"]);
    }
