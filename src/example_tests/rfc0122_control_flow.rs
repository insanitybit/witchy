use super::*;

/// Returning a place selected by control flow must carry the selected runtime
/// place, rather than a copy or the function's temporary return slot.
#[test]
fn conditional_exclusive_reference_return_preserves_each_runtime_place_on_both_backends() {
    let src = r#"mode opt

type Pair:
    left: Int
    right: Int

fn select(pair: &'a mut Pair, first: Bool) -> &'a mut Int:
    if first:
        &mut pair.left
    else:
        &mut pair.right

fn main(console: Console):
    var first = Pair(1, 2)
    let left = select(&mut first, true)
    *left = 9
    console.print("${*left}")
    var second = Pair(3, 4)
    let right = select(&mut second, false)
    *right = 8
    console.print("${*right}")
"#;
    let want = ["9", "8"];

    assert_eq!(link_run(src), want, "interpreter preserves the selected place");
    assert_eq!(wasm_run_reowns(src).0, want, "compiled code preserves the selected place");
}
