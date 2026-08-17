use super::*;

#[test]
fn direct_shared_reference_return_preserves_the_runtime_place_on_both_backends() {
    let src = r#"mode opt

fn first(text: &'a String) -> &'a String:
    text

fn main(console: Console):
    let text = "value"
    let observed = first(&text)
    console.print("${*observed}")
"#;
    let want = ["value"];
    assert_eq!(link_run(src), want, "interpreter preserves the shared returned place");
    assert_eq!(wasm_run_reowns(src).0, want, "compiled backend preserves the shared returned place");
}

#[test]
fn mutable_to_shared_reference_return_preserves_the_runtime_place_on_both_backends() {
    let src = r#"mode opt

fn share(text: &'a mut String) -> &'a String:
    text

fn main(console: Console):
    var text = "value"
    let editable = &mut text
    let observed = share(editable)
    console.print("${*observed}")
"#;
    let want = ["value"];
    assert_eq!(link_run(src), want, "interpreter preserves the shared reborrow");
    assert_eq!(wasm_run_reowns(src).0, want, "compiled backend preserves the shared reborrow");
}

#[test]
fn var_exclusive_reference_parameter_writes_the_owner_on_both_backends() {
    let src = r#"mode opt

fn replace(var text: &'a mut String) -> Nil:
    *text = "updated"

fn main(console: Console):
    var text = "value"
    replace(&mut text)
    console.print(text)
"#;
    let want = ["updated"];
    assert_eq!(link_run(src), want, "interpreter writes through a var exclusive-reference parameter");
    assert_eq!(wasm_run_reowns(src).0, want, "compiled backend writes through a var exclusive-reference parameter");
}

#[test]
fn shared_reference_return_preserves_the_runtime_place_on_both_backends() {
    let src = "mode opt\n\nfn first(text: &'a String) -> &'a String:\n    text\n\nfn main(console: Console):\n    let text = \"value\"\n    let observed = first(&text)\n    console.print(\"${*observed}\")\n";
    let want = ["value"];
    assert_eq!(link_run(src), want, "interpreter preserves a directly borrowed shared place");
    assert_eq!(wasm_run_reowns(src).0, want, "compiled backend preserves a directly borrowed shared place");
}

#[test]
fn function_value_shared_reference_return_preserves_the_runtime_place_on_both_backends() {
    let src = r#"mode opt

fn first(text: &'a String) -> &'a String:
    text

fn main(console: Console):
    var text = "value"
    let project = first
    let observed = project(&text)
    console.print(*observed)
"#;
    let want = ["value"];
    assert_eq!(link_run(src), want, "interpreter preserves its function-value reference carrier");
    assert_eq!(wasm_run_reowns(src).0, want, "compiled function value preserves its returned reference carrier");
}

#[test]
fn closure_shared_reference_return_preserves_the_runtime_place_on_both_backends() {
    let src = r#"mode opt

fn main(console: Console):
    var text = "value"
    let project = fn(text: &'a String) -> &'a String:
        text
    let observed = project(&text)
    console.print(*observed)
"#;
    let want = ["value"];
    assert_eq!(link_run(src), want, "interpreter preserves its closure reference carrier");
    assert_eq!(wasm_run_reowns(src).0, want, "compiled closure preserves its returned reference carrier");
}

#[test]
fn function_value_exclusive_reference_return_writes_the_owner_on_both_backends() {
    let src = r#"mode opt

fn first(text: &'a mut String) -> &'a mut String:
    text

fn main(console: Console):
    var text = "value"
    let project = first
    let observed = project(&mut text)
    *observed = "updated"
    console.print(text)
"#;
    let want = ["updated"];
    assert_eq!(link_run(src), want, "interpreter writes through its function-value exclusive reference carrier");
    assert_eq!(wasm_run_reowns(src).0, want, "compiled function value writes through its returned exclusive reference carrier");
}

#[test]
fn unique_exclusive_reference_return_writes_the_owner_on_both_backends() {
    let src = r#"mode opt

fn pass(own input: unique &'a mut String) -> unique &'a mut String:
    input

fn main(console: Console):
    var text = "before"
    let editable = &mut text
    let returned = pass(editable)
    *returned = "after"
    console.print(text)
"#;
    let want = ["after"];
    assert_eq!(
        link_run(src),
        want,
        "interpreter preserves a consumed unique exclusive-reference carrier",
    );
    assert_eq!(
        wasm_run_reowns(src).0,
        want,
        "compiled code preserves a consumed unique exclusive-reference carrier",
    );
}

#[test]
fn unique_exclusive_reference_function_value_return_writes_the_owner_on_both_backends() {
    let src = r#"mode opt

fn pass(own input: unique &'a mut String) -> unique &'a mut String:
    input

fn main(console: Console):
    var text = "before"
    let project = pass
    let editable = &mut text
    let returned = project(editable)
    *returned = "after"
    console.print(text)
"#;
    let want = ["after"];
    assert_eq!(
        link_run(src),
        want,
        "interpreter preserves a consumed unique reference through a function value",
    );
    assert_eq!(
        wasm_run_reowns(src).0,
        want,
        "compiled code preserves a consumed unique reference through a function value",
    );
}

#[test]
fn explicit_exclusive_reference_return_writes_the_owner_on_both_backends() {
    let src = r#"mode opt

fn select(input: &'a mut String, first: Bool) -> &'a mut String:
    if first:
        return input
    input

fn main(console: Console):
    var text = "before"
    let editable = select(&mut text, true)
    *editable = "after"
    console.print(text)
"#;
    let want = ["after"];
    assert_eq!(
        link_run(src),
        want,
        "interpreter transfers an explicit exclusive-reference return",
    );
    assert_eq!(
        wasm_run_reowns(src).0,
        want,
        "compiled code transfers an explicit exclusive-reference return",
    );
}

#[test]
fn function_value_mutable_to_shared_reborrow_preserves_the_owner_on_both_backends() {
    let src = r#"mode opt

fn share(text: &'a mut String) -> &'a String:
    text

fn main(console: Console):
    var text = "value"
    let adapt = share
    let editable = &mut text
    let observed = adapt(editable)
    console.print(*observed)
"#;
    let want = ["value"];
    assert_eq!(link_run(src), want, "interpreter preserves a function-value mutable-to-shared reborrow carrier");
    assert_eq!(wasm_run_reowns(src).0, want, "compiled function value preserves a mutable-to-shared reborrow carrier");
}

#[test]
fn closure_mutable_to_shared_reborrow_preserves_the_owner_on_both_backends() {
    let src = r#"mode opt

fn main(console: Console):
    var text = "value"
    let share = fn(value: &'a mut String) -> &'a String:
        value
    let editable = &mut text
    let observed = share(editable)
    console.print(*observed)
"#;
    let want = ["value"];
    assert_eq!(link_run(src), want, "interpreter shortens an exclusive closure argument to shared access");
    assert_eq!(wasm_run_reowns(src).0, want, "compiled code shortens an exclusive closure argument to shared access");
}

#[test]
fn shared_reference_tuple_preserves_each_owner_root_on_both_backends() {
    let src = r#"mode opt

fn pair(left: &'a String, right: &'b String) -> (&'a String, &'b String):
    (left, right)

fn main(console: Console):
    var first = "first"
    var second = "second"
    let tuple = pair(&first, &second)
    let copied = tuple
    let (left, right) = copied
    console.print(*left)
    console.print(*right)
    console.print(*tuple.0)
    console.print(*tuple.1)
"#;
    let want = ["first", "second", "first", "second"];
    assert_eq!(link_run(src), want, "interpreter preserves tuple reference owners");
    assert_eq!(wasm_run_reowns(src).0, want, "compiled backend preserves tuple reference owners");
}

#[test]
fn shared_reference_list_preserves_each_owner_root_on_both_backends() {
    let src = r#"mode opt

import list

fn all(left: &'a String, right: &'a String) -> List(&'a String):
    [left, right]

fn main(console: Console):
    var first = "first"
    var second = "second"
    let values = all(&first, &second)
    console.print(*values[0])
    console.print(*values[1])
"#;
    let want = ["first", "second"];
    assert_eq!(link_run(src), want, "interpreter preserves list reference owners");
    assert_eq!(wasm_run_reowns(src).0, want, "compiled backend preserves list reference owners");
}

#[test]
fn exclusive_reference_list_projection_writes_the_selected_owner_on_both_backends() {
    let src = r#"mode opt

import list

fn all(left: &'a mut String, right: &'a mut String) -> List(&'a mut String):
    [left, right]

fn main(console: Console):
    var first = "first"
    var second = "second"
    let values = all(&mut first, &mut second)
    *values[1] = "updated"
    console.print(first)
    console.print(second)
"#;
    let want = ["first", "updated"];
    assert_eq!(link_run(src), want, "interpreter writes through the selected list reference");
    assert_eq!(wasm_run_reowns(src).0, want, "compiled backend writes through the selected list reference");
}

#[test]
fn exclusive_reference_list_move_then_projection_writes_the_selected_owner_on_both_backends() {
    let src = r#"mode opt

import list

fn all(left: &'a mut String, right: &'a mut String) -> List(&'a mut String):
    [left, right]

fn main(console: Console):
    var first = "first"
    var second = "second"
    let values = all(&mut first, &mut second)
    let moved = values
    *moved[1] = "updated"
    console.print(first)
    console.print(second)
"#;
    let want = ["first", "updated"];
    assert_eq!(
        link_run(src),
        want,
        "interpreter preserves a moved exclusive list carrier for projection",
    );
    assert_eq!(
        wasm_run_reowns(src).0,
        want,
        "compiled backend preserves a moved exclusive list carrier for projection",
    );
}

#[test]
fn exclusive_reference_list_iteration_writes_each_owner_on_both_backends() {
    let src = r#"mode opt

import list

fn all(left: &'a mut String, right: &'a mut String) -> List(&'a mut String):
    [left, right]

fn main(console: Console):
    var first = "first"
    var second = "second"
    let values = all(&mut first, &mut second)
    for value in values:
        *value = "updated"
    console.print(first)
    console.print(second)
"#;
    let want = ["updated", "updated"];
    assert_eq!(
        link_run(src),
        want,
        "interpreter preserves exclusive references through list iteration",
    );
    assert_eq!(
        wasm_run_reowns(src).0,
        want,
        "compiled backend preserves exclusive references through list iteration",
    );
}

#[test]
fn exclusive_reference_list_iteration_reborrows_and_resumes_each_element_on_both_backends() {
    let src = r#"mode opt

import list

fn all(left: &'a mut String, right: &'a mut String) -> List(&'a mut String):
    [left, right]

fn main(console: Console):
    var first = "first"
    var second = "second"
    let values = all(&mut first, &mut second)
    for value in values:
        let child = &mut *value
        *child = "child"
        *value = "resumed"
    console.print(first)
    console.print(second)
"#;
    let want = ["resumed", "resumed"];
    assert_eq!(
        link_run(src),
        want,
        "interpreter resumes each list-element reference after its reborrow ends",
    );
    assert_eq!(
        wasm_run_reowns(src).0,
        want,
        "compiled backend resumes each list-element reference after its reborrow ends",
    );
}

#[test]
fn exclusive_reference_tuple_destructure_writes_the_selected_owner_on_both_backends() {
    let src = r#"mode opt

fn pair(left: &'a mut String, right: &'b mut String) -> (&'a mut String, &'b mut String):
    (left, right)

fn main(console: Console):
    var first = "first"
    var second = "second"
    let values = pair(&mut first, &mut second)
    let (left, right) = values
    *right = "updated"
    console.print(*left)
    console.print(second)
"#;
    let want = ["first", "updated"];
    assert_eq!(link_run(src), want, "interpreter writes through the destructured tuple reference");
    assert_eq!(wasm_run_reowns(src).0, want, "compiled backend writes through the destructured tuple reference");
}
