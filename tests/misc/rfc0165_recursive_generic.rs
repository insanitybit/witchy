//! Concrete generic recursive nominals must carry their instantiated layout and
//! ownership through the source interpreter, canonical oracle, and admitted Wasm.

#[test]
fn recursive_generic_stack_int_and_string_agree_across_three_legs() {
    let source = r#"
type Stack:
    Empty
    Push(a, Stack(a))

fn sum_int(stack: Stack(Int)) -> Int:
    match stack:
        Empty -> 0
        Push(value, rest) -> value + sum_int(rest)

fn string_weight(stack: Stack(String)) -> Int:
    match stack:
        Empty -> 0
        Push(value, rest) -> value.length() + string_weight(rest)

fn main(console: Console):
    let ints = Push(1, Push(1, Empty))
    let words = Push("x", Push("yy", Empty))
    console.print("${sum_int(ints) + string_weight(words)}")
"#;
    assert_three_legs(source, vec!["5".to_owned()]);
}

#[test]
fn match_shadowed_reference_stays_local_and_outer_mutation_survives() {
    assert_three_legs(r#"
fn main(console: Console):
    let decoded: Option(Int) = Some(42)
    var count = 0
    let result = match decoded:
        Some(decoded) ->
            count = count + 1
            decoded
        None -> 0
    let original = match decoded:
        Some(value) -> value
        None -> 0
    console.print("${result}")
    console.print("${original}")
    console.print("${count}")
"#, vec!["42".to_owned(), "42".to_owned(), "1".to_owned()]);
}

#[test]
fn effect_match_with_only_returns_has_no_join_value() {
    assert_three_legs(r#"
fn choose(value: Option(Int), flag: Bool) -> Int:
    if flag:
        match value:
            Some(n) ->
                return n
            None ->
                return 2
    0

fn main(console: Console):
    console.print("${choose(Some(7), true)}")
    console.print("${choose(None, true)}")
    console.print("${choose(None, false)}")
"#, vec!["7".to_owned(), "2".to_owned(), "0".to_owned()]);
}

#[test]
fn indexed_string_and_empty_string_result_join_preserves_ownership() {
    assert_three_legs(r#"
fn first_or_empty(parts: List(String), use_first: Bool) -> String:
    let first = if use_first:
        parts[0]
    else:
        ""
    first

fn main(console: Console):
    let parts = ["held"]
    console.print(first_or_empty(parts, true))
    console.print("[${first_or_empty(parts, false)}]")
    console.print(parts[0])
"#, vec!["held".to_owned(), "[]".to_owned(), "held".to_owned()]);
}

#[test]
fn typed_closure_family_carries_scalar_and_list_captures_across_three_legs() {
    assert_three_legs(r#"
fn apply(f: fn(Int) -> Int, value: Int) -> Int:
    f(value)

fn choose(first: Bool, left: fn(Int) -> Int, right: fn(Int) -> Int) -> fn(Int) -> Int:
    if first:
        left
    else:
        right

fn main(console: Console):
    let offset = 7
    let values = [2, 3]
    let scalar = fn(value: Int): value + offset
    let list = fn(value: Int): value + values[0] + values[1]
    let selected = choose(false, scalar, list)
    console.print("${apply(scalar, 5)}")
    console.print("${apply(list, 5)}")
    console.print("${apply(selected, 5)}")
"#, vec!["12".to_owned(), "10".to_owned(), "10".to_owned()]);
}

#[test]
fn owned_recursive_aggregate_moves_through_calls_and_reclaims() {
    assert_three_legs(r#"
type Stack:
    Empty
    Push(Int, Stack)

fn consume(own stack: Stack) -> Int:
    match stack:
        Empty -> 0
        Push(value, rest) -> value + consume(rest)

fn forward(own stack: Stack) -> Int:
    consume(stack)

fn main(console: Console):
    let stack = Push(2, Push(5, Empty))
    console.print("${forward(stack)}")
"#, vec!["7".to_owned()]);
}

#[test]
fn typed_closure_zero_capture_uses_its_non_adapter_family_tag() {
    assert_three_legs(r#"
fn main(console: Console):
    let offset = 7
    let captured = fn(value: Int): value + offset
    let empty = fn(value: Int): value + 5
    console.print("${captured(5)}")
    console.print("${empty(5)}")
"#, vec!["12".to_owned(), "10".to_owned()]);
}

#[test]
fn recursive_closure_family_stores_typed_function_capture_and_reclaims() {
    assert_three_legs(r#"
fn compose(f: fn(Int) -> Int) -> fn(Int) -> Int:
    fn(value: Int): f(value)

fn main(console: Console):
    let increment = fn(value: Int): value + 1
    let composed = compose(increment)
    console.print("${composed(4)}")
"#, vec!["5".to_owned()]);
}

#[test]
fn function_fields_and_function_lists_construct_call_and_reclaim() {
    assert_three_legs(r#"
type FnBox:
    FnBox(fn(Int) -> Int)

fn apply_box(box: FnBox, value: Int) -> Int:
    match box:
        FnBox(f) -> f(value)

fn apply_second(functions: List(fn(Int) -> Int), value: Int) -> Int:
    let f = functions[1]
    f(value)

fn main(console: Console):
    let boxed = FnBox(fn(value: Int): value + 1)
    let functions = [fn(value: Int): value + 1, fn(value: Int): value * 2]
    console.print("${apply_box(boxed, 4)}")
    console.print("${apply_second(functions, 4)}")
"#, vec!["5".to_owned(), "8".to_owned()]);
}

#[test]
fn owned_function_argument_moves_through_calls_and_reclaims() {
    assert_three_legs(r#"
fn invoke(own f: fn(Int) -> Int, value: Int) -> Int:
    f(value)

fn forward(own f: fn(Int) -> Int, value: Int) -> Int:
    invoke(f, value)

fn main(console: Console):
    let offset = 3
    let f = fn(value: Int): value + offset
    console.print("${forward(f, 4)}")
"#, vec!["7".to_owned()]);
}

#[test]
fn owned_function_with_string_capture_reclaims_when_branch_skips_invocation() {
    assert_three_legs(r#"
fn consume(own f: fn(Int) -> Int, invoke: Bool) -> Int:
    if invoke:
        f(4)
    else:
        0

fn main(console: Console):
    let suffix = "xx"
    let skipped = fn(value: Int): value + suffix.length()
    let invoked = fn(value: Int): value + suffix.length()
    console.print("${consume(skipped, false)}")
    console.print("${consume(invoked, true)}")
"#, vec!["0".to_owned(), "6".to_owned()]);
}

#[test]
fn anonymous_record_reflect_field_uses_its_concrete_instantiation() {
    assert_three_legs(r#"
import reflect
from json import Json

fn main(console: Console):
    console.print(json.encode(Json.from(.{x: 7})))
    console.print(json.encode(Json.from(.{x: "seven"})))
"#, vec![r#"{"x":7}"#.to_owned(), r#"{"x":"seven"}"#.to_owned()]);
}

#[test]
fn tuple_binding_in_value_block_preserves_owned_values_and_effects() {
    assert_three_legs(r#"
fn choose(flag: Bool, console: Console) -> String:
    let result = if flag:
        let (name, number) = ("selected", 7)
        let _ = console.print("effect")
        "${name}:${number}"
    else:
        let (name, number) = ("other", 9)
        "${name}:${number}"
    result

fn main(console: Console):
    console.print(choose(true, console))
    console.print(choose(false, console))
"#, vec!["effect".to_owned(), "selected:7".to_owned(), "other:9".to_owned()]);
}

#[test]
fn jq_recursive_json_queries_execute_across_three_legs() {
    let expected = [
        ("user.name", r#""Ada""#),
        ("user.roles", r#"["admin","dev"]"#),
        ("user.roles.0", r#""admin""#),
        ("user.roles.1", r#""dev""#),
        ("count", "42"),
        ("active", "true"),
        ("user.missing", "(no such path)"),
    ].into_iter().map(|(path, result)| format!("{path:<16}=> {result}")).collect();
    assert_three_legs_with_config(
        include_str!("../../examples/jq/src/jq.witchy"),
        expected,
        witchy_wir::canonical_interpreter::InterpreterConfig {
            max_steps: 20_000_000,
            ..Default::default()
        },
    );
}

#[test]
fn queens_shared_terminal_blocks_are_emitted_once() {
    assert_three_legs_with_config(include_str!("../../examples/queens/src/queens.witchy"), vec![
        "8-queens solutions: 92".to_owned(),
        "first solution:".to_owned(),
        "Q.......".to_owned(),
        "....Q...".to_owned(),
        ".......Q".to_owned(),
        ".....Q..".to_owned(),
        "..Q.....".to_owned(),
        "......Q.".to_owned(),
        ".Q......".to_owned(),
        "...Q....".to_owned(),
    ], witchy_wir::canonical_interpreter::InterpreterConfig {
        max_steps: 20_000_000,
        ..Default::default()
    });
}

#[test]
fn generic_templates_do_not_shift_executable_tail_site_owners() {
    assert_three_legs(include_str!("../../examples/generic_stack/src/generic_stack.witchy"), vec![
        "nums size:  3".to_owned(),
        "words size: 2".to_owned(),
        "nums top:   1".to_owned(),
        "words top:  first".to_owned(),
        "rev nums top:  3".to_owned(),
        "rev words top: second".to_owned(),
    ]);
}

#[test]
fn list_concat_preserves_owned_inputs_across_three_legs() {
    assert_three_legs(r#"
fn scalar() -> Int:
    let left = [1, 2]
    let right = [3, 4]
    let joined = left.concat(right)
    joined[0] + joined[3] + left[1] + right[0]

fn strings() -> Int:
    let left = ["a", "bb"]
    let right = ["ccc"]
    let joined = left.concat(right)
    joined[0].length() + joined[2].length() + left[1].length() + right[0].length()

fn main(console: Console):
    console.print("${scalar()}")
    console.print("${strings()}")
"#, vec!["10".to_owned(), "9".to_owned()]);
}

#[test]
fn recursive_lists_and_tuple_fields_execute_and_reclaim() {
    assert_three_legs(r#"
type Tree:
    Leaf
    Array(List(Tree))
    Object(List((String, Tree)))

fn main(console: Console):
    let leaf = Leaf
    let array = Array([leaf, leaf])
    let object = Object([("items", array)])
    match object:
        Object(pairs) ->
            let (name, value) = pairs[0]
            match value:
                Array(items) -> console.print("${name}: ${items.length()}")
                _ -> console.print("unexpected value")
        _ -> console.print("unexpected object")
"#, vec!["items: 2".to_owned()]);
}

#[test]
fn valued_if_continues_only_with_the_surviving_value() {
    assert_three_legs(r#"
fn scalar(flag: Bool) -> Int:
    let value = if flag:
        return 7
    else:
        10
    value + 1

fn text(flag: Bool) -> String:
    let value = if flag:
        return "early"
    else:
        "late"
    value + "!"

fn main(console: Console):
    console.print("${scalar(true)}")
    console.print("${scalar(false)}")
    console.print(text(true))
    console.print(text(false))
"#, vec!["7".to_owned(), "11".to_owned(), "early".to_owned(), "late!".to_owned()]);
}

fn assert_three_legs(source: &str, expected: Vec<String>) {
    assert_three_legs_with_config(source, expected, Default::default());
}

fn assert_three_legs_with_config(
    source: &str,
    expected: Vec<String>,
    config: witchy_wir::canonical_interpreter::InterpreterConfig,
) {
    let checked = witchy::resolve_std_only_checked(source)
        .expect("recursive generic source checks");
    let source_output = witchy::interpreter::run_module_args(
        checked.module().clone(),
        ".",
        Vec::new(),
        Vec::new(),
    )
    .expect("source interpreter executes both concrete Stack instantiations");
    assert_eq!(source_output, expected);

    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("recursive generic source admits");
    let entry = artifact.entry().expect("main entry");
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry.export().strip_prefix('f').expect("canonical export prefix").parse().expect("function id"),
    );
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked)
        .expect("oracle validation");
    let capabilities = entry.capabilities().iter().copied()
        .map(witchy_wir::canonical_interpreter::InterpreterValue::entry_capability)
        .collect();
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated,
        function,
        capabilities,
        config,
    )
    .expect("canonical oracle executes both concrete Stack instantiations");
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0);

    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        witchy::runtime::Capabilities {
            print: true,
            quiet: true,
            ..Default::default()
        },
        256,
    )
    .expect("admitted instance");
    vm.run().expect("Wasm executes both concrete Stack instantiations");
    assert_eq!(vm.output(), expected);
    assert_eq!(
        vm.canonical_heap_leaks().expect("heap audit").expect("canonical artifact"),
        0,
    );
}
