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

/// Explicit reference returns keep their owner through early, branch, loop, and
/// `?` return paths on both execution backends.
#[test]
fn reference_return_control_flow_preserves_the_runtime_place_on_both_backends() {
    let src = r#"mode opt

fn branch(input: &'a String, take_early: Bool) -> &'a String:
    if take_early:
        return input
    input

fn looped(input: &'a String) -> &'a String:
    var count = 0
    while (count < 2):
        count = count + 1
    return input

fn result(value: Int, should_fail: Bool) -> Result(Int, String):
    if should_fail:
        Err("stop")
    else:
        Ok(value)

fn tried(input: &'a Int, should_fail: Bool) -> Result(Int, String):
    let observed = input
    let value = result(1, should_fail)?
    Ok(*observed + value)

fn main(console: Console):
    let input = "value"
    console.print(*branch(&input, true))
    console.print(*branch(&input, false))
    console.print(*looped(&input))
    var number = 41
    match tried(&number, false):
        Ok(value) -> console.print("${value}")
        Err(message) -> console.print(message)
    match tried(&number, true):
        Ok(value) -> console.print("${value}")
        Err(message) -> console.print(message)
"#;
    let want = ["value", "value", "value", "42", "stop"];

    assert_eq!(link_run(src), want, "interpreter preserves return-path references");
    assert_eq!(wasm_run_reowns(src).0, want, "compiled code preserves return-path references");
}
