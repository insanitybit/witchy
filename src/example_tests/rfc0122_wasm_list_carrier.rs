use super::*;

/// Keep the list carrier executable while its interpreter representation is
/// still unsettled. This fixture deliberately covers the complete Wasm-first
/// path: returned aggregate, local binding, reference copy, and projection.
#[test]
fn shared_reference_list_return_copy_and_projection_work_on_wasm() {
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

    assert_eq!(
        wasm_run_reowns(src).0,
        ["first", "second"],
        "compiled Wasm preserves the returned list reference carrier through copy and projection",
    );
}

/// Keep nominal aggregate transport on the same Wasm-first carrier path while
/// the interpreter representation is still being converged.
#[test]
fn shared_reference_nominal_aggregate_return_copy_and_projection_work_on_wasm() {
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

    assert_eq!(
        wasm_run_reowns(src).0,
        ["first", "second"],
        "compiled Wasm preserves a nominal aggregate reference carrier through copy and projection",
    );
}
