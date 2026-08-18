use super::*;

#[test]
fn wasm_first_exclusive_reference_list_function_value_return_preserves_forced_copy() {
    let src = r#"mode opt

fn make(
    own left: unique &'a mut String,
    own right: unique &'a mut String,
) -> List((unique &'a mut String, unique &'a mut String)):
    [(left, right)]

fn main(console: Console):
    var first = "first"
    var second = "second"
    let project = make
    let returned = project(&mut first, &mut second)
    let (left, right) = returned[0]
    *left = "left updated"
    *right = "right updated"
    console.print(first)
    console.print(second)
"#;
    let want = ["left updated", "right updated"];
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm preserves a two-owner callable list carrier");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves a two-owner callable list carrier");
}

#[test]
fn interpreter_exclusive_reference_list_function_value_return_preserves_writeback() {
    let src = r#"mode opt

fn make(
    own left: unique &'a mut String,
    own right: unique &'a mut String,
) -> List((unique &'a mut String, unique &'a mut String)):
    [(left, right)]

fn main(console: Console):
    var first = "first"
    var second = "second"
    let project = make
    let returned = project(&mut first, &mut second)
    let (left, right) = returned[0]
    *left = "left updated"
    *right = "right updated"
    console.print(first)
    console.print(second)
"#;

    assert_eq!(
        link_run(src),
        ["left updated", "right updated"],
        "interpreter preserves a two-owner callable list carrier",
    );
}
