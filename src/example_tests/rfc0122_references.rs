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
fn closure_exclusive_reference_return_writes_the_owner_on_all_backends() {
    let src = r#"mode opt

fn main(console: Console):
    var text = "value"
    let project = fn(text: &'a mut String) -> &'a mut String:
        text
    let observed = project(&mut text)
    *observed = "updated"
    console.print(text)
"#;
    let want = ["updated"];
    assert_eq!(link_run(src), want, "interpreter writes through its closure exclusive-reference carrier");
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm writes through its closure exclusive-reference carrier");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves its closure exclusive-reference carrier");
}

#[test]
fn closure_unique_exclusive_reference_return_writes_the_owner_on_all_backends() {
    let src = r#"mode opt

fn main(console: Console):
    var text = "value"
    let project = fn(own text: unique &'a mut String) -> unique &'a mut String:
        text
    let observed = project(&mut text)
    *observed = "updated"
    console.print(text)
"#;
    let want = ["updated"];
    assert_eq!(link_run(src), want, "interpreter preserves a unique closure exclusive-reference carrier");
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm preserves a unique closure exclusive-reference carrier");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves a unique closure exclusive-reference carrier");
}

#[test]
fn unique_exclusive_reference_callable_field_preserves_its_contract_on_all_backends() {
    let src = r#"mode opt

type Holder:
    project: fn(own unique &'a mut String) -> unique &'a mut String

fn pass(own value: unique &'a mut String) -> unique &'a mut String:
    value

fn main(console: Console):
    var text = "before"
    let holder = Holder(project: pass)
    let project = holder.project
    let observed = project(&mut text)
    *observed = "after"
    console.print(text)
"#;
    let want = ["after"];
    assert_eq!(link_run(src), want, "interpreter preserves the unique callable field contract");
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm preserves the unique callable field contract");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves the unique callable field contract");
}

#[test]
fn wasm_first_exclusive_reference_option_return_writes_selected_owner() {
    let src = r#"mode opt

fn choose(value: &'a mut String, selected: Bool) -> Option(&'a mut String):
    if selected:
        Some(value)
    else:
        None

fn main(console: Console):
    var text = "before"
    var fallback = "fallback"
    let selected = choose(&mut text, true)
    let value = selected ?? &mut fallback
    *value = "after"
    console.print(text)
"#;
    let want = ["after"];
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm preserves an Option exclusive-reference carrier");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves an Option exclusive-reference carrier");
}

#[test]
fn wasm_first_exclusive_reference_result_return_writes_selected_owner() {
    let src = r#"mode opt

fn choose(value: &'a mut String, selected: Bool) -> Result(&'a mut String, String):
    if selected:
        Ok(value)
    else:
        Err("not selected")

fn main(console: Console):
    var text = "before"
    var fallback = "fallback"
    let selected = choose(&mut text, true)
    let value = selected ?? &mut fallback
    *value = "after"
    console.print(text)
"#;
    let want = ["after"];
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm preserves a Result exclusive-reference carrier");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves a Result exclusive-reference carrier");
}

#[test]
fn unique_exclusive_reference_callable_tuple_projection_preserves_its_contract_on_all_backends() {
    let src = r#"mode opt

fn pass(own value: unique &'a mut String) -> unique &'a mut String:
    value

fn main(console: Console):
    var text = "before"
    let carrier = (pass, 1)
    let project = carrier.0
    let observed = project(&mut text)
    *observed = "after"
    console.print(text)
"#;
    let want = ["after"];
    assert_eq!(link_run(src), want, "interpreter preserves the unique callable tuple contract");
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm preserves the unique callable tuple contract");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves the unique callable tuple contract");
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
fn generic_exclusive_reference_function_value_return_writes_the_owner_on_all_backends() {
    let src = r#"mode opt

fn identity(value: &'a mut a) -> &'a mut a:
    value

fn main(console: Console):
    var text = "before"
    let project = identity
    let returned = project(&mut text)
    *returned = "after"
    console.print(text)
"#;
    let want = ["after"];
    assert_eq!(link_run(src), want, "interpreter preserves a generic exclusive reference carrier");
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm preserves a generic exclusive reference carrier");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves a generic exclusive reference carrier");
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
fn unique_exclusive_reference_list_function_value_return_writes_the_owner_on_all_backends() {
    let src = r#"mode opt

fn make(own input: unique &'a mut String) -> List(unique &'a mut String):
    [input]

fn main(console: Console):
    var text = "before"
    let project = make
    let returned = project(&mut text)
    let selected = returned[0]
    *selected = "after"
    console.print(text)
"#;
    let want = ["after"];
    assert_eq!(link_run(src), want, "interpreter preserves a unique reference through a list function value");
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm preserves a unique reference through a list function value");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves a unique reference through a list function value");
}

#[test]
fn unique_exclusive_reference_callable_list_projection_preserves_its_contract_on_all_backends() {
    let src = r#"mode opt

import list

fn pass(own value: unique &'a mut String) -> unique &'a mut String:
    value

fn main(console: Console):
    var text = "before"
    let carrier = [pass]
    let project = carrier[0]
    let observed = project(&mut text)
    *observed = "after"
    console.print(text)
"#;
    let want = ["after"];
    assert_eq!(link_run(src), want, "interpreter preserves the unique callable list contract");
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm preserves the unique callable list contract");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves the unique callable list contract");
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
fn generic_mutable_to_shared_reborrow_preserves_the_owner_on_all_backends() {
    let src = r#"mode opt

fn share(value: &'a mut a) -> &'a a:
    value

fn main(console: Console):
    var text = "value"
    let observed = share(&mut text)
    console.print(*observed)
"#;
    let want = ["value"];
    assert_eq!(link_run(src), want, "interpreter shortens a generic exclusive reference to shared access");
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm preserves a generic shared reborrow");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves a generic shared reborrow");
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
fn exclusive_reference_loop_reborrow_writes_and_restores_parent_on_all_backends() {
    let src = r#"mode opt

fn main(console: Console):
    var text = "before"
    let parent = &mut text
    var count = 0
    while count < 1:
        let child = &mut *parent
        *child = "inside"
        let _ = child
        count = count + 1
    *parent = "after"
    console.print(text)
"#;
    let want = ["after"];
    assert_eq!(link_run(src), want, "interpreter resumes the parent after a loop reborrow");
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm resumes the parent after a loop reborrow");
    assert_eq!(forced_copy, want, "forced-copy Wasm resumes the parent after a loop reborrow");
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

#[test]
fn exclusive_reference_list_carrier_survives_forced_copy_wasm() {
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
    let want = link_run(src);
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm agrees with the interpreter");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves the place carrier");
}
