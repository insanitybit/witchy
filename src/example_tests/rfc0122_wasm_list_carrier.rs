use super::*;

/// Keep the list carrier executable across both backends. This fixture covers
/// the complete carrier path: returned aggregate, local binding, reference
/// copy, and projection.
#[test]
fn shared_reference_list_return_copy_and_projection_work_on_both_backends() {
    let src = r#"mode opt

import list

fn all(left: &'a String, right: &'a String) -> List(&'a String):
    [left, right]

fn main(console: Console):
    var first = "first"
    var second = "second"
    let returned = all(&first, &second)
    let copied = returned
    let left = copied[0]
    let right = copied[1]
    console.print(*left)
    console.print(*right)
"#;

    let want = ["first", "second"];
    assert_eq!(
        link_run(src),
        want,
        "interpreter preserves the returned list reference carrier through copy and projection",
    );
    assert_eq!(
        wasm_run_reowns(src).0,
        want,
        "compiled Wasm preserves the returned list reference carrier through copy and projection",
    );
}

/// Keep nominal aggregate transport on the same carrier path across both
/// backends.
#[test]
fn shared_reference_nominal_aggregate_return_copy_and_projection_work_on_both_backends() {
    let src = r#"mode opt

type Pair('a):
    left: &'a String
    right: &'a String

fn pair(left: &'a String, right: &'a String) -> Pair('a):
    Pair(left, right)

fn main(console: Console):
    var first = "first"
    var second = "second"
    let returned = pair(&first, &second)
    let copied = returned
    console.print(*(copied.left))
    console.print(*(copied.right))
"#;

    let want = ["first", "second"];
    assert_eq!(
        link_run(src),
        want,
        "interpreter preserves a nominal aggregate reference carrier through copy and projection",
    );
    assert_eq!(
        wasm_run_reowns(src).0,
        want,
        "compiled Wasm preserves a nominal aggregate reference carrier through copy and projection",
    );
}

/// Keep affine nominal aggregate transport on the same carrier path across
/// both backends.
#[test]
fn exclusive_reference_nominal_aggregate_move_destructure_and_write_work_on_both_backends() {
    let src = r#"mode opt

type Pair('a, 'b):
    left: &'a mut String
    right: &'b mut String

fn pair(left: &'a mut String, right: &'b mut String) -> Pair('a, 'b):
    Pair(left, right)

fn main(console: Console):
    var first = "first"
    var second = "second"
    let returned = pair(&mut first, &mut second)
    let moved = returned
    let Pair(left, right) = moved
    *left = "updated-first"
    *right = "updated-second"
    console.print(first)
    console.print(second)
"#;

    let want = ["updated-first", "updated-second"];
    assert_eq!(
        link_run(src),
        want,
        "interpreter preserves an exclusive nominal aggregate through move, destructure, and writes",
    );
    assert_eq!(
        wasm_run_reowns(src).0,
        want,
        "compiled Wasm preserves an exclusive nominal aggregate through move, destructure, and writes",
    );
}

/// Keep nested nominal/list transport on the same carrier path across both
/// backends.
#[test]
fn shared_reference_nested_nominal_list_return_copy_and_projection_work_on_both_backends() {
    let src = r#"mode opt

import list

type Pair('a, 'b):
    left: &'a String
    right: &'b String

fn pairs(left: &'a String, right: &'b String) -> List(Pair('a, 'b)):
    [Pair(left, right)]

fn main(console: Console):
    var first = "first"
    var second = "second"
    let returned = pairs(&first, &second)
    let copied = returned
    let pair = copied[0]
    console.print(*(pair.left))
    console.print(*(pair.right))
"#;

    let want = ["first", "second"];
    assert_eq!(
        link_run(src),
        want,
        "interpreter preserves nested nominal/list reference carriers through return, copy, and projection",
    );
    assert_eq!(
        wasm_run_reowns(src).0,
        want,
        "compiled Wasm preserves nested nominal/list reference carriers through return, copy, and projection",
    );
}

/// Keep nested nominal/list exclusive transport on the same carrier path across
/// both backends.
#[test]
fn exclusive_reference_nested_nominal_list_move_projection_and_write_work_on_both_backends() {
    let src = r#"mode opt

import list

type Pair('a, 'b):
    left: &'a mut String
    right: &'b mut String

fn pairs(left: &'a mut String, right: &'b mut String) -> List(Pair('a, 'b)):
    [Pair(left, right)]

fn main(console: Console):
    var first = "first"
    var second = "second"
    let returned = pairs(&mut first, &mut second)
    let moved = returned
    let pair = moved[0]
    *pair.left = "updated-first"
    *pair.right = "updated-second"
    console.print(first)
    console.print(second)
"#;

    let want = ["updated-first", "updated-second"];
    assert_eq!(
        link_run(src),
        want,
        "interpreter preserves nested exclusive nominal/list carriers through move, projection, and writes",
    );
    assert_eq!(
        wasm_run_reowns(src).0,
        want,
        "compiled Wasm preserves nested exclusive nominal/list carriers through move, projection, and writes",
    );
}

#[test]
fn exclusive_reference_list_extract_then_project_disjoint_owners_on_both_backends() {
    let src = r#"mode opt

fn main(console: Console):
    var first = "first"
    var second = "second"
    let returned: List(&'a mut String) = [&mut first, &mut second]
    let moved = returned
    let selected = moved[0]
    *selected = "updated-first"
    *moved[1] = "updated-second"
    console.print(first)
    console.print(second)
"#;

    let want = ["updated-first", "updated-second"];
    assert_eq!(
        link_run(src),
        want,
        "interpreter preserves an unrelated exclusive list projection after extracting one element",
    );
    assert_eq!(
        wasm_run_reowns(src).0,
        want,
        "compiled Wasm preserves an unrelated exclusive list projection after extracting one element",
    );
}

/// Keep a tuple reference carrier on the same path across both backends.
#[test]
fn shared_reference_tuple_return_copy_and_projection_work_on_both_backends() {
    let src = r#"mode opt

fn pair(left: &'a String, right: &'b String) -> (&'a String, &'b String):
    (left, right)

fn main(console: Console):
    var first = "first"
    var second = "second"
    let returned = pair(&first, &second)
    let copied = returned
    let left = copied.0
    let right = copied.1
    console.print(*left)
    console.print(*right)
"#;

    let want = ["first", "second"];
    assert_eq!(
        link_run(src),
        want,
        "interpreter preserves a tuple reference carrier through return, copy, and projection",
    );
    assert_eq!(
        wasm_run_reowns(src).0,
        want,
        "compiled Wasm preserves a tuple reference carrier through return, copy, and projection",
    );
}

/// Keep an affine tuple reference carrier on the same path across both
/// backends.
#[test]
fn exclusive_reference_tuple_move_projection_and_write_work_on_both_backends() {
    let src = r#"mode opt

fn pair(left: &'a mut String, right: &'b mut String) -> (&'a mut String, &'b mut String):
    (left, right)

fn main(console: Console):
    var first = "first"
    var second = "second"
    let returned = pair(&mut first, &mut second)
    let moved = returned
    let left = moved.0
    let right = moved.1
    *left = "updated-first"
    *right = "updated-second"
    console.print(first)
    console.print(second)
"#;

    let want = ["updated-first", "updated-second"];
    assert_eq!(
        link_run(src),
        want,
        "interpreter preserves an affine tuple reference carrier through move, projection, and writes",
    );
    assert_eq!(
        wasm_run_reowns(src).0,
        want,
        "compiled Wasm preserves an affine tuple reference carrier through move, projection, and writes",
    );
}
