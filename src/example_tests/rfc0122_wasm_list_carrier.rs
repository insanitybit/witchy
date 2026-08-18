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

/// Keep list element projection on the compiled-Wasm carrier path while the
/// aggregate branch/result ABI is still settling. The selected list element
/// must remain the same executable place after a conditional tail.
#[test]
fn wasm_first_exclusive_reference_list_conditional_projection_writes_selected_owner() {
    let src = r#"mode opt

import list

fn choose(values: &'a mut List(Int), left: Bool) -> &'a mut Int:
    let selected = if left:
        &mut values[0]
    else:
        &mut values[1]
    selected

fn main(console: Console):
    var values = [1, 2]
    let selected = choose(&mut values, false)
    *selected = 9
    console.print("${values}")
    console.print("${*selected}")
"#;

    let want = ["[1, 9]", "9"];
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm preserves a selected list element place through a conditional tail");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves a selected list element place through a conditional tail");
}

#[test]
fn interpreter_exclusive_reference_list_conditional_projection_writes_selected_owner() {
    let src = r#"mode opt

import list

fn choose(values: &'a mut List(Int), left: Bool) -> &'a mut Int:
    let selected = if left:
        &mut values[0]
    else:
        &mut values[1]
    selected

fn main(console: Console):
    var values = [1, 2]
    let selected = choose(&mut values, false)
    *selected = 9
    console.print("${values}")
    console.print("${*selected}")
"#;

    assert_eq!(
        link_run(src),
        ["[1, 9]", "9"],
        "interpreter preserves a selected list element place through a conditional tail",
    );
}

/// Keep the callable result ABI on the same compiled-Wasm-first list carrier
/// path: the function value selects the projected place inside its conditional
/// body, then the caller writes through the returned handle.
#[test]
fn wasm_first_exclusive_reference_list_conditional_function_value_writes_selected_owner() {
    let src = r#"mode opt

import list

fn choose(values: &'a mut List(Int), left: Bool) -> &'a mut Int:
    let selected = if left:
        &mut values[0]
    else:
        &mut values[1]
    selected

fn main(console: Console):
    var values = [1, 2]
    let project = choose
    let selected = project(&mut values, false)
    *selected = 9
    console.print("${values}")
    console.print("${*selected}")
"#;

    let want = ["[1, 9]", "9"];
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm preserves a conditional list place through a function value");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves a conditional list place through a function value");
}

#[test]
fn interpreter_exclusive_reference_list_conditional_function_value_writes_selected_owner() {
    let src = r#"mode opt

import list

fn choose(values: &'a mut List(Int), left: Bool) -> &'a mut Int:
    let selected = if left:
        &mut values[0]
    else:
        &mut values[1]
    selected

fn main(console: Console):
    var values = [1, 2]
    let project = choose
    let selected = project(&mut values, false)
    *selected = 9
    console.print("${values}")
    console.print("${*selected}")
"#;

    assert_eq!(
        link_run(src),
        ["[1, 9]", "9"],
        "interpreter preserves a conditional list place through a function value",
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

#[test]
fn wasm_first_exclusive_reference_option_nominal_match_writes_both_owners() {
    let src = r#"mode opt

type Pair('a, 'b):
    left: &'a mut String
    right: &'b mut String

fn choose(left: &'a mut String, right: &'b mut String) -> Option(Pair('a, 'b)):
    Some(Pair(left, right))

fn main(console: Console):
    var first = "first"
    var second = "second"
    let selected = choose(&mut first, &mut second)
    match selected:
        Some(pair) ->
            let Pair(left, right) = pair
            *left = "left updated"
            *right = "right updated"
        None -> console.print("unexpected")
    console.print(first)
    console.print(second)
"#;
    let want = ["left updated", "right updated"];
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm preserves an Option nominal carrier");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves an Option nominal carrier");
}

#[test]
fn interpreter_exclusive_reference_option_nominal_match_writes_both_owners() {
    let src = r#"mode opt

type Pair('a, 'b):
    left: &'a mut String
    right: &'b mut String

fn choose(left: &'a mut String, right: &'b mut String) -> Option(Pair('a, 'b)):
    Some(Pair(left, right))

fn main(console: Console):
    var first = "first"
    var second = "second"
    let selected = choose(&mut first, &mut second)
    match selected:
        Some(pair) ->
            let Pair(left, right) = pair
            *left = "left updated"
            *right = "right updated"
        None -> console.print("unexpected")
    console.print(first)
    console.print(second)
"#;

    assert_eq!(
        link_run(src),
        ["left updated", "right updated"],
        "interpreter preserves an Option nominal carrier",
    );
}

#[test]
fn wasm_first_exclusive_reference_option_nominal_list_match_projects_and_writes() {
    let src = r#"mode opt

type Pair('a, 'b):
    left: &'a mut String
    right: &'b mut String

fn choose(left: &'a mut String, right: &'b mut String) -> Option(List(Pair('a, 'b))):
    Some([Pair(left, right)])

fn main(console: Console):
    var first = "first"
    var second = "second"
    let selected = choose(&mut first, &mut second)
    match selected:
        Some(values) ->
            let pair = values[0]
            let Pair(left, right) = pair
            *left = "left updated"
            *right = "right updated"
        None -> console.print("unexpected")
    console.print(first)
    console.print(second)
"#;
    let want = ["left updated", "right updated"];
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm preserves an Option nominal list carrier");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves an Option nominal list carrier");
}

#[test]
fn interpreter_exclusive_reference_option_nominal_list_match_projects_and_writes() {
    let src = r#"mode opt

type Pair('a, 'b):
    left: &'a mut String
    right: &'b mut String

fn choose(left: &'a mut String, right: &'b mut String) -> Option(List(Pair('a, 'b))):
    Some([Pair(left, right)])

fn main(console: Console):
    var first = "first"
    var second = "second"
    let selected = choose(&mut first, &mut second)
    match selected:
        Some(values) ->
            let pair = values[0]
            let Pair(left, right) = pair
            *left = "left updated"
            *right = "right updated"
        None -> console.print("unexpected")
    console.print(first)
    console.print(second)
"#;

    assert_eq!(
        link_run(src),
        ["left updated", "right updated"],
        "interpreter preserves an Option nominal list carrier",
    );
}

#[test]
fn wasm_first_unique_exclusive_reference_option_nominal_list_preserves_qualifiers() {
    let src = r#"mode opt

type Pair('a, 'b):
    left: unique &'a mut String
    right: unique &'b mut String

fn choose(
    left: unique &'a mut String,
    right: unique &'b mut String,
) -> Option(List(Pair('a, 'b))):
    Some([Pair(left, right)])

fn main(console: Console):
    var first = "first"
    var second = "second"
    let selected = choose(&mut first, &mut second)
    match selected:
        Some(values) ->
            let pair = values[0]
            let Pair(left, right) = pair
            *left = "left updated"
            *right = "right updated"
        None -> console.print("unexpected")
    console.print(first)
    console.print(second)
"#;
    let want = ["left updated", "right updated"];
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm preserves unique qualifiers in an Option nominal list");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves unique qualifiers in an Option nominal list");
}

#[test]
fn interpreter_unique_exclusive_reference_option_nominal_list_preserves_qualifiers() {
    let src = r#"mode opt

type Pair('a, 'b):
    left: unique &'a mut String
    right: unique &'b mut String

fn choose(
    left: unique &'a mut String,
    right: unique &'b mut String,
) -> Option(List(Pair('a, 'b))):
    Some([Pair(left, right)])

fn main(console: Console):
    var first = "first"
    var second = "second"
    let selected = choose(&mut first, &mut second)
    match selected:
        Some(values) ->
            let pair = values[0]
            let Pair(left, right) = pair
            *left = "left updated"
            *right = "right updated"
        None -> console.print("unexpected")
    console.print(first)
    console.print(second)
"#;

    assert_eq!(
        link_run(src),
        ["left updated", "right updated"],
        "interpreter preserves unique qualifiers in an Option nominal list",
    );
}

#[test]
fn wasm_first_exclusive_reference_option_nominal_list_none_branch_preserves_owners() {
    let src = r#"mode opt

type Pair('a, 'b):
    left: &'a mut String
    right: &'b mut String

fn choose(
    enabled: Bool,
    left: &'a mut String,
    right: &'b mut String,
) -> Option(List(Pair('a, 'b))):
    if enabled:
        Some([Pair(left, right)])
    else:
        None

fn main(console: Console):
    var first = "first"
    var second = "second"
    let selected = choose(false, &mut first, &mut second)
    match selected:
        Some(_) -> console.print("unexpected")
        None -> console.print("none")
    console.print(first)
    console.print(second)
"#;
    let want = ["none", "first", "second"];
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm preserves a None nominal list carrier");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves a None nominal list carrier");
}

#[test]
fn interpreter_exclusive_reference_option_nominal_list_none_branch_preserves_owners() {
    let src = r#"mode opt

type Pair('a, 'b):
    left: &'a mut String
    right: &'b mut String

fn choose(
    enabled: Bool,
    left: &'a mut String,
    right: &'b mut String,
) -> Option(List(Pair('a, 'b))):
    if enabled:
        Some([Pair(left, right)])
    else:
        None

fn main(console: Console):
    var first = "first"
    var second = "second"
    let selected = choose(false, &mut first, &mut second)
    match selected:
        Some(_) -> console.print("unexpected")
        None -> console.print("none")
    console.print(first)
    console.print(second)
"#;

    assert_eq!(
        link_run(src),
        ["none", "first", "second"],
        "interpreter preserves owners when an Option nominal list is None",
    );
}

#[test]
fn unique_exclusive_reference_nominal_field_move_and_write_work_on_all_backends() {
    let src = r#"mode opt

type Holder('a):
    value: unique &'a mut String

fn wrap(own value: unique &'a mut String) -> Holder('a):
    Holder(value)

fn main(console: Console):
    var text = "before"
    let returned = wrap(&mut text)
    let moved = returned
    let Holder(value) = moved
    *value = "after"
    console.print(text)
"#;
    let want = ["after"];
    assert_eq!(link_run(src), want, "interpreter preserves a unique nominal reference field");
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm preserves a unique nominal reference field");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves a unique nominal reference field");
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

/// This is deliberately compiled-Wasm-first while the nested aggregate
/// carrier ABI is still changing. The interpreter debt is recorded in the
/// RFC-0122 acceptance ledger rather than making this lowering slice wait.
#[test]
fn wasm_first_nested_exclusive_tuple_list_carrier_writes_after_destructure() {
    let src = r#"mode opt

import list

fn pair(first: &'a mut String, second: &'b mut String) -> List((&'a mut String, &'b mut String)):
    [(first, second)]

fn main(console: Console):
    var first = "first"
    var second = "second"
    let returned = pair(&mut first, &mut second)
    let moved = returned
    let (left, right) = moved[0]
    *left = "updated-first"
    *right = "updated-second"
    console.print(first)
    console.print(second)
"#;

    assert_eq!(
        wasm_run_reowns(src).0,
        ["updated-first", "updated-second"],
        "compiled Wasm preserves an affine tuple-in-list carrier through return, move, destructure, projection, and writes",
    );
}

#[test]
fn interpreter_nested_exclusive_tuple_list_carrier_writes_after_destructure() {
    let src = r#"mode opt

import list

fn pair(first: &'a mut String, second: &'b mut String) -> List((&'a mut String, &'b mut String)):
    [(first, second)]

fn main(console: Console):
    var first = "first"
    var second = "second"
    let returned = pair(&mut first, &mut second)
    let moved = returned
    let (left, right) = moved[0]
    *left = "updated-first"
    *right = "updated-second"
    console.print(first)
    console.print(second)
"#;

    assert_eq!(
        link_run(src),
        ["updated-first", "updated-second"],
        "interpreter preserves an affine tuple-in-list carrier through return, move, destructure, projection, and writes",
    );
}

#[test]
fn exclusive_reference_nested_tuple_list_carrier_preserves_forced_copy() {
    let src = r#"mode opt

import list

fn pair(first: &'a mut String, second: &'b mut String) -> List((&'a mut String, &'b mut String)):
    [(first, second)]

fn main(console: Console):
    var first = "first"
    var second = "second"
    let returned = pair(&mut first, &mut second)
    let moved = returned
    let (left, right) = moved[0]
    *left = "updated-first"
    *right = "updated-second"
    console.print(first)
    console.print(second)
"#;
    let want = link_run(src);
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);

    assert_eq!(optimized, want, "optimized Wasm preserves nested tuple-list transport");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves nested tuple-list transport");
}

/// Exercise replacement of an exclusive place inside a reference list. This
/// remains Wasm-first while list slot write-back is still part of the changing
/// carrier ABI; its interpreter debt is recorded in RFC-0122's ledger.
#[test]
fn wasm_first_exclusive_reference_list_slot_replacement_preserves_writeback() {
    let src = r#"mode opt

import list

fn main(console: Console):
    var first = "first"
    var second = "second"
    var replacement = "replacement"
    var refs: List(&'a mut String) = [&mut first, &mut second]
    list.set_at(refs, 0, &mut replacement)
    let selected = refs[0]
    *selected = "updated-replacement"
    console.print(first)
    console.print(second)
    console.print(replacement)
"#;

    assert_eq!(
        wasm_run_reowns(src).0,
        ["first", "second", "updated-replacement"],
        "compiled Wasm preserves exclusive list slot replacement and write-back",
    );
}

#[test]
fn interpreter_exclusive_reference_list_slot_replacement_preserves_writeback() {
    let src = r#"mode opt

import list

fn main(console: Console):
    var first = "first"
    var second = "second"
    var replacement = "replacement"
    var refs: List(&'a mut String) = [&mut first, &mut second]
    list.set_at(refs, 0, &mut replacement)
    let selected = refs[0]
    *selected = "updated-replacement"
    console.print(first)
    console.print(second)
    console.print(replacement)
"#;

    assert_eq!(
        link_run(src),
        ["first", "second", "updated-replacement"],
        "interpreter preserves exclusive list slot replacement and write-back",
    );
}

#[test]
fn exclusive_reference_list_slot_replacement_preserves_forced_copy() {
    let src = r#"mode opt

import list

fn main(console: Console):
    var first = "first"
    var second = "second"
    var replacement = "replacement"
    var refs: List(&'a mut String) = [&mut first, &mut second]
    list.set_at(refs, 0, &mut replacement)
    let selected = refs[0]
    *selected = "updated-replacement"
    console.print(first)
    console.print(second)
    console.print(replacement)
"#;
    let want = link_run(src);
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);

    assert_eq!(optimized, want, "optimized Wasm preserves list slot replacement");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves list slot replacement");
}

#[test]
fn exclusive_reference_list_push_preserves_forced_copy() {
    let src = r#"mode opt

import list

fn main(console: Console):
    var first = "first"
    var second = "second"
    var refs: List(&'a mut String) = [&mut first]
    list.push(refs, &mut second)
    let selected = refs[1]
    *selected = "updated-second"
    console.print(first)
    console.print(second)
"#;
    let want = link_run(src);
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);

    assert_eq!(optimized, want, "optimized Wasm preserves list push");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves list push");
}

#[test]
fn exclusive_reference_empty_list_push_preserves_forced_copy() {
    let src = r#"mode opt

import list

fn main(console: Console):
    var second = "second"
    var refs: List(&'a mut String) = []
    list.push(refs, &mut second)
    let selected = refs[0]
    *selected = "updated-second"
    console.print(second)
"#;
    let want = link_run(src);
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);

    assert_eq!(optimized, want, "optimized Wasm preserves empty-list push");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves empty-list push");
}

/// Exercise construction of a mutable reference list through the ordinary
/// list append path. This remains Wasm-first while the list carrier ABI is
/// changing; its interpreter debt is recorded in RFC-0122's ledger.
#[test]
fn wasm_first_exclusive_reference_list_push_preserves_writeback() {
    let src = r#"mode opt

import list

fn main(console: Console):
    var first = "first"
    var second = "second"
    var refs: List(&'a mut String) = [&mut first]
    list.push(refs, &mut second)
    let selected = refs[1]
    *selected = "updated-second"
    console.print(first)
    console.print(second)
"#;

    assert_eq!(
        wasm_run_reowns(src).0,
        ["first", "updated-second"],
        "compiled Wasm preserves exclusive reference-list push and write-back",
    );
}

#[test]
fn wasm_first_exclusive_reference_empty_list_push_preserves_writeback() {
    let src = r#"mode opt

import list

fn main(console: Console):
    var second = "second"
    var refs: List(&'a mut String) = []
    list.push(refs, &mut second)
    let selected = refs[0]
    *selected = "updated-second"
    console.print(second)
"#;

    assert_eq!(
        wasm_run_reowns(src).0,
        ["updated-second"],
        "compiled Wasm preserves exclusive reference-list push from an empty typed carrier",
    );
}

/// Converge the list-push carrier on the interpreter once the Wasm ABI slice
/// is executable. The ledger keeps this named parity fixture separate from
/// the Wasm-first implementation evidence.
#[test]
fn interpreter_exclusive_reference_list_push_preserves_writeback() {
    let src = r#"mode opt

import list

fn main(console: Console):
    var first = "first"
    var second = "second"
    var refs: List(&'a mut String) = [&mut first]
    list.push(refs, &mut second)
    let selected = refs[1]
    *selected = "updated-second"
    console.print(first)
    console.print(second)
"#;

    assert_eq!(
        link_run(src),
        ["first", "updated-second"],
        "interpreter preserves exclusive reference-list push and write-back",
    );
}

#[test]
fn interpreter_exclusive_reference_empty_list_push_preserves_writeback() {
    let src = r#"mode opt

import list

fn main(console: Console):
    var second = "second"
    var refs: List(&'a mut String) = []
    list.push(refs, &mut second)
    let selected = refs[0]
    *selected = "updated-second"
    console.print(second)
"#;

    assert_eq!(
        link_run(src),
        ["updated-second"],
        "interpreter preserves exclusive reference-list push from an empty typed carrier",
    );
}

/// Keep local nullable aggregate construction on the compiled-Wasm-first path:
/// the carrier is built in an `if`, moved, matched, destructured, projected,
/// and written through without first crossing a helper-function ABI.
#[test]
fn wasm_first_local_nullable_exclusive_tuple_list_constructs_and_writes() {
    let src = r#"mode opt

fn main(console: Console):
    var first = "first"
    var second = "second"
    let selected = if true:
        Some([(&mut first, &mut second)])
    else:
        None
    let moved = selected
    match moved:
        Some(values) ->
            let (left, right) = values[0]
            *left = "left updated"
            *right = "right updated"
        None -> console.print("unexpected")
    console.print(first)
    console.print(second)
"#;
    let want = ["left updated", "right updated"];
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(
        optimized,
        want,
        "optimized Wasm preserves a locally constructed nullable carrier"
    );
    assert_eq!(
        forced_copy,
        want,
        "forced-copy Wasm preserves a locally constructed nullable carrier"
    );
}

/// A reference-list registry entry must not reclassify an ordinary scalar
/// `List(String)` that shares the qualifier-erased source key.
#[test]
fn scalar_list_remains_scalar_when_a_reference_list_has_the_same_element_type() {
    let src = r#"mode opt

fn main(console: Console):
    var first = "first"
    let views = [&first]
    let scalar_values = ["scalar"]
    let view = views[0]
    let scalar = scalar_values[0]
    console.print(*view)
    console.print(scalar)
"#;
    let want = ["first", "scalar"];
    codegen::set_force_copy_for_tests(None);
    let optimized = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(Some(true));
    let forced_copy = wasm_run_reowns(src).0;
    codegen::set_force_copy_for_tests(None);
    assert_eq!(optimized, want, "optimized Wasm keeps scalar and reference lists distinct");
    assert_eq!(forced_copy, want, "forced-copy Wasm keeps scalar and reference lists distinct");
}
