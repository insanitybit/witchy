//! A `let` argument lent to a callee leaves the caller's binding unique, so
//! later in-place updates must not reach what any callee kept: values seen by
//! a retaining callee keep their state at the call.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

const SOURCE: &str = r#"
fn keep(let d: Dict(String, Int), var out: List(Dict(String, Int))):
    out.push(d)

fn size_of(let d: Dict(String, Int)) -> Int:
    d.length()

fn main(console: Console):
    var d: Dict(String, Int) = dict.with_capacity(4)
    let _ = d.insert("a", 1)
    var kept: List(Dict(String, Int)) = []
    var i = 0
    while i < 3:
        keep(d, kept)
        let n = d.get("a") ?? 0
        let _ = d.insert("a", n + 1)
        let _ = d.insert("k${i}", i)
        console.print("${size_of(d)} ${d.get_or("a", 0)}")
        i = i + 1
    var j = 0
    while j < kept.length():
        console.print("kept ${kept[j].length()} ${kept[j].get_or("a", 0)}")
        j = j + 1
"#;

fn output_of(source: &str) -> Vec<String> {
    let checked = witchy::resolve_std_only_checked(source).expect("check program");
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("compile program");
    let mut runtime = Runtime::batch().expect("create runtime");
    let mut actor = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, print_int: true, quiet: true, ..Default::default() }, 128)
        .expect("spawn program");
    actor.run().expect("run program");
    actor.output().to_vec()
}

#[test]
fn a_dictionary_kept_by_a_callee_does_not_see_later_in_place_updates() {
    assert_eq!(output_of(SOURCE), [
        "2 2", "3 3", "4 4",
        "kept 1 1", "kept 2 2", "kept 3 3",
    ]);
}

// Each returned list is a share of `xs` as it stood at the call: 1, 11, 21
// with lengths 1, 2, 3, so the total is 3200 + 106 = 3306.
const RETURNED_SHARE: &str = r#"
fn g(xs: List(Int)) -> List(Int):
    xs

fn f(let xs: List(Int)) -> List(Int):
    g(xs)

fn main(console: Console):
    var xs: List(Int) = [1]
    var i = 0
    var total = 0
    while i < 3:
        let ys = f(xs)
        xs[0] = xs[0] + 10
        xs.push(i)
        total = total + ys[0] * 100 + ys.length()
        i = i + 1
    console.print("${total}")
"#;

#[test]
fn a_list_returned_from_a_let_borrow_does_not_see_later_in_place_updates() {
    assert_eq!(output_of(RETURNED_SHARE), ["3306"]);
}

const WRITTEN_BACK_SHARE: &str = r#"
fn g(xs: List(Int)) -> List(Int):
    xs

fn f(let xs: List(Int), var out: List(Int)):
    out = g(xs)

fn main(console: Console):
    var xs: List(Int) = [1]
    var out: List(Int) = []
    var i = 0
    var total = 0
    while i < 3:
        f(xs, out)
        xs[0] = xs[0] + 10
        xs.push(i)
        total = total + out[0] * 100 + out.length()
        i = i + 1
    console.print("${total}")
"#;

#[test]
fn a_list_written_back_from_a_let_borrow_does_not_see_later_in_place_updates() {
    assert_eq!(output_of(WRITTEN_BACK_SHARE), ["3306"]);
}
