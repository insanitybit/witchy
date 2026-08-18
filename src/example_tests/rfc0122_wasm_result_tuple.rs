use super::*;

#[test]
fn wasm_first_exclusive_reference_result_tuple_match_writes_both_owners() {
    let src = r#"mode opt

fn choose(
    left: &'a mut String,
    right: &'a mut String,
    selected: Bool,
) -> Result((&'a mut String, &'a mut String), String):
    if selected:
        Ok((left, right))
    else:
        Err("not selected")

fn main(console: Console):
    var first = "first"
    var second = "second"
    let selected = choose(&mut first, &mut second, true)
    match selected:
        Ok(pair) ->
            let (left, right) = pair
            *left = "left updated"
            *right = "right updated"
        Err(_) -> console.print("unexpected")
    console.print(first)
    console.print(second)
"#;
    let want = ["left updated", "right updated"];
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm preserves a tagged Result tuple carrier");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves a tagged Result tuple carrier");
}
