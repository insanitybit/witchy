use super::*;

/// Keep an exclusive reference-list return on the same Wasm-first carrier as
/// list construction. The interpreter differential is recorded in RFC-0122
/// until the call/result ABI is frozen.
#[test]
fn wasm_first_exclusive_reference_list_return_move_projection_writeback() {
    let src = r#"mode opt

fn make(text: &'a mut String) -> List(&'a mut String):
    [text]

fn main(console: Console):
    var text = "before"
    let returned = make(&mut text)
    let moved = returned
    let selected = moved[0]
    *selected = "after"
    console.print(text)
"#;

    assert_eq!(
        wasm_run_reowns(src).0,
        ["after"],
        "compiled Wasm preserves an exclusive reference-list return through move, projection, and write-back",
    );
}

#[test]
fn interpreter_exclusive_reference_list_return_move_projection_writeback() {
    let src = r#"mode opt

fn make(text: &'a mut String) -> List(&'a mut String):
    [text]

fn main(console: Console):
    var text = "before"
    let returned = make(&mut text)
    let moved = returned
    let selected = moved[0]
    *selected = "after"
    console.print(text)
"#;

    assert_eq!(
        link_run(src),
        ["after"],
        "interpreter preserves an exclusive reference-list return through move, projection, and write-back",
    );
}

#[test]
fn exclusive_reference_list_return_preserves_forced_copy() {
    let src = r#"mode opt

fn make(text: &'a mut String) -> List(&'a mut String):
    [text]

fn main(console: Console):
    var text = "before"
    let returned = make(&mut text)
    let moved = returned
    let selected = moved[0]
    *selected = "after"
    console.print(text)
"#;
    let want = link_run(src);
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);

    assert_eq!(optimized, want, "optimized Wasm preserves direct list return");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves direct list return");
}

#[test]
fn wasm_first_exclusive_reference_list_function_value_return_writeback() {
    let src = r#"mode opt

fn make(text: &'a mut String) -> List(&'a mut String):
    [text]

fn main(console: Console):
    var text = "before"
    let factory = make
    let returned = factory(&mut text)
    let selected = returned[0]
    *selected = "after"
    console.print(text)
"#;

    assert_eq!(
        wasm_run_reowns(src).0,
        ["after"],
        "compiled Wasm preserves an exclusive reference-list return through a function value",
    );
}

#[test]
fn wasm_first_exclusive_reference_list_function_value_return_preserves_forced_copy() {
    let src = r#"mode opt

fn make(text: &'a mut String) -> List(&'a mut String):
    [text]

fn main(console: Console):
    var text = "before"
    let factory = make
    let returned = factory(&mut text)
    let selected = returned[0]
    *selected = "after"
    console.print(text)
"#;
    let want = link_run(src);
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);

    assert_eq!(optimized, want, "optimized Wasm preserves the function-value list carrier");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves the function-value list carrier");
}
