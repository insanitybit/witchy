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

#[test]
fn wasm_first_exclusive_reference_result_list_match_projects_and_writes() {
    let src = r#"mode opt

fn choose(
    left: &'a mut String,
    right: &'a mut String,
) -> Result(List((&'a mut String, &'a mut String)), String):
    Ok([(left, right)])

fn main(console: Console):
    var first = "first"
    var second = "second"
    let selected = choose(&mut first, &mut second)
    match selected:
        Ok(values) ->
            let pair = values[0]
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
    assert_eq!(optimized, want, "optimized Wasm preserves a Result list tuple carrier");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves a Result list tuple carrier");
}

#[test]
fn wasm_first_exclusive_reference_result_nominal_match_writes_both_owners() {
    let src = r#"mode opt

type Pair('a, 'b):
    left: &'a mut String
    right: &'b mut String

fn choose(left: &'a mut String, right: &'b mut String) -> Result(Pair('a, 'b), String):
    Ok(Pair(left, right))

fn main(console: Console):
    var first = "first"
    var second = "second"
    let selected = choose(&mut first, &mut second)
    match selected:
        Ok(pair) ->
            let Pair(left, right) = pair
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
    assert_eq!(optimized, want, "optimized Wasm preserves a Result nominal carrier");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves a Result nominal carrier");
}

#[test]
fn interpreter_exclusive_reference_result_nominal_match_writes_both_owners() {
    let src = r#"mode opt

type Pair('a, 'b):
    left: &'a mut String
    right: &'b mut String

fn choose(left: &'a mut String, right: &'b mut String) -> Result(Pair('a, 'b), String):
    Ok(Pair(left, right))

fn main(console: Console):
    var first = "first"
    var second = "second"
    let selected = choose(&mut first, &mut second)
    match selected:
        Ok(pair) ->
            let Pair(left, right) = pair
            *left = "left updated"
            *right = "right updated"
        Err(_) -> console.print("unexpected")
    console.print(first)
    console.print(second)
"#;

    assert_eq!(
        link_run(src),
        ["left updated", "right updated"],
        "interpreter preserves a Result nominal carrier",
    );
}

#[test]
fn wasm_first_exclusive_reference_option_list_match_projects_and_writes() {
    let src = r#"mode opt

fn choose(
    left: &'a mut String,
    right: &'a mut String,
) -> Option(List((&'a mut String, &'a mut String))):
    Some([(left, right)])

fn main(console: Console):
    var first = "first"
    var second = "second"
    let selected = choose(&mut first, &mut second)
    match selected:
        Some(values) ->
            let pair = values[0]
            let (left, right) = pair
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
    assert_eq!(optimized, want, "optimized Wasm preserves an Option list tuple carrier");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves an Option list tuple carrier");
}

#[test]
fn wasm_first_exclusive_reference_option_list_none_branch_preserves_owners() {
    let src = r#"mode opt

fn choose(
    enabled: Bool,
    left: &'a mut String,
    right: &'a mut String,
) -> Option(List((&'a mut String, &'a mut String))):
    if enabled:
        Some([(left, right)])
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
    assert_eq!(optimized, want, "optimized Wasm preserves a None Option list carrier");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves a None Option list carrier");
}

#[test]
fn wasm_first_exclusive_reference_option_tuple_match_writes_both_owners() {
    let src = r#"mode opt

fn choose(
    left: &'a mut String,
    right: &'a mut String,
) -> Option((&'a mut String, &'a mut String)):
    Some((left, right))

fn main(console: Console):
    var first = "first"
    var second = "second"
    let selected = choose(&mut first, &mut second)
    match selected:
        Some(pair) ->
            let (left, right) = pair
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
    assert_eq!(optimized, want, "optimized Wasm preserves an Option tuple carrier");
    assert_eq!(forced_copy, want, "forced-copy Wasm preserves an Option tuple carrier");
}

#[test]
fn interpreter_exclusive_reference_option_tuple_match_writes_both_owners() {
    let src = r#"mode opt

fn choose(
    left: &'a mut String,
    right: &'a mut String,
) -> Option((&'a mut String, &'a mut String)):
    Some((left, right))

fn main(console: Console):
    var first = "first"
    var second = "second"
    let selected = choose(&mut first, &mut second)
    match selected:
        Some(pair) ->
            let (left, right) = pair
            *left = "left updated"
            *right = "right updated"
        None -> console.print("unexpected")
    console.print(first)
    console.print(second)
"#;

    assert_eq!(
        link_run(src),
        ["left updated", "right updated"],
        "interpreter preserves an Option tuple carrier",
    );
}

#[test]
fn interpreter_exclusive_reference_option_list_match_projects_and_writes() {
    let src = r#"mode opt

fn choose(
    left: &'a mut String,
    right: &'a mut String,
) -> Option(List((&'a mut String, &'a mut String))):
    Some([(left, right)])

fn main(console: Console):
    var first = "first"
    var second = "second"
    let selected = choose(&mut first, &mut second)
    match selected:
        Some(values) ->
            let pair = values[0]
            let (left, right) = pair
            *left = "left updated"
            *right = "right updated"
        None -> console.print("unexpected")
    console.print(first)
    console.print(second)
"#;

    assert_eq!(
        link_run(src),
        ["left updated", "right updated"],
        "interpreter preserves a nullable Option list tuple carrier",
    );
}

#[test]
fn interpreter_exclusive_reference_result_tuple_match_writes_both_owners() {
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

    assert_eq!(
        link_run(src),
        ["left updated", "right updated"],
        "interpreter preserves a tagged Result tuple carrier",
    );
}

#[test]
fn interpreter_exclusive_reference_result_list_match_projects_and_writes() {
    let src = r#"mode opt

fn choose(
    left: &'a mut String,
    right: &'a mut String,
) -> Result(List((&'a mut String, &'a mut String)), String):
    Ok([(left, right)])

fn main(console: Console):
    var first = "first"
    var second = "second"
    let selected = choose(&mut first, &mut second)
    match selected:
        Ok(values) ->
            let pair = values[0]
            let (left, right) = pair
            *left = "left updated"
            *right = "right updated"
        Err(_) -> console.print("unexpected")
    console.print(first)
    console.print(second)
"#;

    assert_eq!(
        link_run(src),
        ["left updated", "right updated"],
        "interpreter preserves a tagged Result list tuple carrier",
    );
}

#[test]
fn interpreter_exclusive_reference_option_list_none_branch_preserves_owners() {
    let src = r#"mode opt

fn choose(
    enabled: Bool,
    left: &'a mut String,
    right: &'a mut String,
) -> Option(List((&'a mut String, &'a mut String))):
    if enabled:
        Some([(left, right)])
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
        "interpreter preserves owners through a nullable None list carrier",
    );
}
