//! Concrete generic recursive nominals must carry their instantiated layout and
//! ownership through the source interpreter, canonical oracle, and admitted Wasm.

#[test]
fn nominal_dictionary_keys_use_declared_equality_across_three_legs() {
    assert_three_legs(r#"
import cmp
import dict

type ById derive(Eq):
    id: Int
    cache: Int

impl PartialEq for ById:
    fn eq(self, other: ById) -> Bool:
        self.id == other.id

type ByCache derive(Eq):
    id: Int
    cache: Int

impl PartialEq for ByCache:
    fn eq(self, other: ByCache) -> Bool:
        self.cache == other.cache

fn main(console: Console):
    var ids: Dict(ById, Int) = dict.new()
    let original = ById(1, 10)
    ids[original] = 1
    let snapshot = ids
    ids[ById(1, 20)] = 2
    ids[ById(2, 20)] = 3
    var caches: Dict(ByCache, Int) = dict.new()
    caches[ByCache(1, 10)] = 4
    caches[ByCache(2, 10)] = 5
    console.print("${ids.length()} ${ids.get_or(ById(1, 30), 0)} ${ids.get_or(ById(3, 20), 0)}")
    console.print("${snapshot.length()} ${snapshot.get_or(ById(1, 99), 0)} ${original.cache}")
    console.print("${caches.length()} ${caches.get_or(ByCache(99, 10), 0)}")
    var scalars: Dict(Int, Int) = dict.new()
    scalars[1] = 6
    scalars[1] = 7
    console.print("${scalars.length()} ${scalars.get_or(1, 0)}")
"#, vec!["2 2 0".into(), "1 1 10".into(), "1 5".into(), "1 7".into()]);
}

#[test]
fn nominal_dictionary_comparators_close_over_nested_dictionary_calls() {
    assert_three_legs(r#"
import cmp
import dict

type Inner derive(Eq):
    id: Int
    cache: Int

impl PartialEq for Inner:
    fn eq(self, other: Inner) -> Bool:
        self.id == other.id

type Outer derive(Eq):
    id: Int

impl PartialEq for Outer:
    fn eq(self, other: Outer) -> Bool:
        var nested: Dict(Inner, Int) = dict.new()
        nested[Inner(self.id, 0)] = 1
        nested.contains_key(Inner(other.id, 99))

fn main(console: Console):
    var values: Dict(Outer, Int) = dict.new()
    values[Outer(4)] = 1
    values[Outer(4)] = 2
    console.print("${values.length()} ${values.get_or(Outer(4), 0)} ${values.get_or(Outer(5), 0)}")
"#, vec!["1 2 0".into()]);
}

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
fn same_family_multi_witness_closures_call_and_reclaim_across_three_legs() {
    let source = r#"
type Node:
    value: Int

fn value_of(node: Node) -> Int:
    node.value

fn main(console: Console):
    let left = [1]
    let right = [2]
    let bare = fn(node: Node): left[0]
    let qualified = fn(node): value_of(node) + right[0]
    console.print("${bare(Node(10))}")
    console.print("${qualified(Node(10))}")
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("checked same-family witnesses");
    let canonical = checked.canonical_source().unwrap();
    let entries = canonical.closure_witness().entries();
    assert_eq!(entries.len(), 2, "both source closures must have checked witnesses");
    assert!(entries.iter().all(|entry| entry.captures().len() == 1));
    let parameters = canonical.closure_witness().entries().iter().filter_map(|entry| {
        let witchy::ast::Type::Fn(parameters, _, _, _) = entry.signature()? else { return None };
        Some(parameters)
    }).collect::<Vec<_>>();
    assert_eq!(parameters.len(), 2);
    assert!(parameters.iter().all(|parameters| matches!(parameters.as_slice(),
        [witchy::ast::Type::Named(name, arguments)] if name == "main.Node" && arguments.is_empty())),
        "the frontend normalizes both witnesses into the same nominal family: {parameters:?}");
    assert_three_legs(source, vec!["1".to_owned(), "12".to_owned()]);
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
fn nested_conditional_list_assignment_preserves_winning_extent_lanes() {
    let source = r#"
fn identity(values: List(Int)) -> List(Int):
    values

fn main(console: Console):
    var result: List(Int) = []
    var found = false
    var index = 0
    while index < 3 && !found:
        if index == 1:
            let attempt = identity([7, 8])
            if attempt.length() == 2:
                result = attempt
                found = true
        index = index + 1
    console.print("${result.length()}")
    console.print("${result[0]}")
    console.print("${result[1]}")
"#;
    let expected = vec!["2".to_owned(), "7".to_owned(), "8".to_owned()];
    assert_three_legs(source, expected.clone());
    assert_three_legs(&format!("mode opt\n{source}"), expected);
}

#[test]
fn conditional_aliases_retain_common_extent_targets_on_both_edges() {
    let source = r#"
fn identity(values: List(Int)) -> List(Int):
    values

fn choose(flag: Bool) -> List(Int):
    var a = identity([0])
    var b = identity([1])
    if flag:
        let picked = identity([7])
        a = picked
        b = picked
    else:
        let picked = identity([8])
        a = picked
        b = picked
    if flag:
        a
    else:
        b

fn main(console: Console):
    console.print("${choose(true)[0]}")
    console.print("${choose(false)[0]}")
"#;
    let expected = vec!["7".to_owned(), "8".to_owned()];
    assert_three_legs(source, expected.clone());
    assert_three_legs(&format!("mode opt\n{source}"), expected);
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

#[test]
fn structural_equality_checks_nested_values_and_recursive_instantiations() {
    let source = r#"
type Stack:
    Empty
    Push(a, Stack(a))

fn same(left: Stack(Int), right: Stack(Int)) -> Bool:
    left == right

fn main(console: Console):
    let left = ([1, 2], (3, 4))
    let right = ([1, 2], (3, 4))
    console.print("${left == right}")
    console.print("${([1], [2]) != ([1], [9])}")
    console.print("${same(Push(1, Empty), Push(1, Empty))}")
    console.print("${same(Push(1, Empty), Empty)}")
    console.print("${Push(2, Push(1, Empty)) == Push(2, Push(9, Empty))}")
    console.print("${Push(\"b\", Push(\"a\", Empty)) == Push(\"b\", Push(\"a\", Empty))}")
    console.print("${Push(\"b\", Push(\"a\", Empty)) != Push(\"b\", Push(\"z\", Empty))}")
"#;
    let expected = ["true", "true", "true", "false", "false", "true", "true"]
        .into_iter().map(str::to_owned).collect::<Vec<_>>();
    assert_three_legs(source, expected.clone());
    assert_three_legs(&format!("mode opt\n{source}"), expected);
}

#[test]
fn structural_equality_preserves_custom_payload_equality_and_ieee_nan() {
    let source = r#"
type Key:
    id: Int
    cache: String

impl PartialEq for Key:
    fn eq(self, other: Key) -> Bool:
        self.id == other.id

type Tree:
    Leaf(Key)
    Branch(Tree, Tree)

fn main(console: Console):
    let left = ([Key(1, "left")], (7, "same"))
    let right = ([Key(1, "right")], (7, "same"))
    console.print("${left == right}")
    console.print("${left == ([Key(2, \"left\")], (7, \"same\"))}")
    let tree = Branch(Leaf(Key(1, "a")), Leaf(Key(2, "b")))
    console.print("${tree == Branch(Leaf(Key(1, \"x\")), Leaf(Key(2, \"y\")))}")
    console.print("${tree == Branch(Leaf(Key(1, \"x\")), Leaf(Key(3, \"y\")))}")
    console.print("${tree != Branch(Leaf(Key(1, \"x\")), Leaf(Key(2, \"y\")))}")
    console.print("${tree == Leaf(Key(1, \"a\"))}")
    let nan = 0.0 / 0.0
    let containing_nan = ([nan], 1)
    console.print("${containing_nan == containing_nan}")
    console.print("${containing_nan != containing_nan}")
"#;
    let expected = ["true", "false", "true", "false", "false", "false", "false", "true"]
        .into_iter().map(str::to_owned).collect::<Vec<_>>();
    assert_three_legs(source, expected.clone());
    assert_three_legs(&format!("mode opt\n{source}"), expected);
}

#[test]
fn structural_equality_evaluates_operands_once_and_keeps_the_left_snapshot() {
    let source = r#"
fn left(values: List(Int), console: Console) -> (List(Int), String):
    console.print("left")
    (values, "same")

fn right(var values: List(Int), console: Console) -> (List(Int), String):
    console.print("right")
    values[0] = 9
    (values, "same")

fn main(console: Console):
    var values = [1]
    let snapshot = values
    console.print("${left(values, console) == right(values, console)}")
    console.print("${snapshot[0]} ${values[0]}")
"#;
    let expected = ["left", "right", "false", "1 9"]
        .into_iter().map(str::to_owned).collect::<Vec<_>>();
    assert_three_legs(source, expected.clone());
    assert_three_legs(&format!("mode opt\n{source}"), expected);
}

#[test]
fn structural_equality_only_compares_the_active_variant_payload() {
    let source = r#"
type Poison:
    divisor: Int

impl PartialEq for Poison:
    fn eq(self, other: Poison) -> Bool:
        (1 / self.divisor) == other.divisor

type Choice:
    Absent
    Present(Poison)

fn main(console: Console):
    let absent: Choice = Absent
    let present = Present(Poison(0))
    console.print("${absent == present}")
    console.print("${present == absent}")
    console.print("${absent == absent}")
"#;
    let expected = ["false", "false", "true"]
        .into_iter().map(str::to_owned).collect::<Vec<_>>();
    assert_three_legs(source, expected.clone());
    assert_three_legs(&format!("mode opt\n{source}"), expected);

    // The payload comparison really traps when both variants select it; a
    // comparator that drops custom equality entirely must not pass the probe.
    let active = source.replace("absent == present", "present == present");
    let checked = witchy::resolve_std_only_checked(&active).expect("checked active payload");
    assert!(witchy::interpreter::run_module_args(
        checked.module().clone(), ".", Vec::new(), Vec::new(),
    ).is_err(), "source custom equality divides by zero");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("active payload comparison admits before execution");
    let entry = artifact.entry().expect("main entry");
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry.export().strip_prefix('f').expect("canonical export prefix")
            .parse().expect("function id"),
    );
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked)
        .expect("active payload oracle validates");
    let capabilities = entry.capabilities().iter().copied()
        .map(witchy_wir::canonical_interpreter::InterpreterValue::entry_capability)
        .collect();
    assert!(witchy_wir::canonical_interpreter::interpret_program(
        &validated, function, capabilities, Default::default(),
    ).is_err(), "oracle executes the trapping payload comparator");
    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        witchy::runtime::Capabilities {
            print: true, quiet: true, ..Default::default()
        },
        256,
    ).expect("admitted active payload instance");
    assert!(vm.run().is_err(), "Wasm executes the trapping payload comparator");
}

#[test]
fn structural_equality_preserves_nested_dictionary_entry_order() {
    let source = r#"
import dict

fn main(console: Console):
    var left: Dict(String, Int) = dict.new()
    left["a"] = 1
    left["b"] = 2
    var reversed: Dict(String, Int) = dict.new()
    reversed["b"] = 2
    reversed["a"] = 1
    var same: Dict(String, Int) = dict.new()
    same["a"] = 1
    same["b"] = 2
    console.print("${([left], 0) == ([reversed], 0)}")
    console.print("${([left], 0) == ([same], 0)}")
    console.print("${([left], 0) != ([reversed], 0)}")
"#;
    let expected = ["false", "true", "true"]
        .into_iter().map(str::to_owned).collect::<Vec<_>>();
    assert_three_legs(source, expected.clone());
    assert_three_legs(&format!("mode opt\n{source}"), expected);
}

#[test]
fn structural_equality_closes_new_dictionary_comparator_dependencies() {
    let source = r#"
import cmp
import dict

type Key derive(Eq):
    id: Int
    cache: Int

impl PartialEq for Key:
    fn eq(self, other: Key) -> Bool:
        self.id == other.id

type Box:
    Box(a)

impl PartialEq for Box(a) where a: PartialEq:
    fn eq(self, other: Self) -> Bool:
        var seen: Dict(Key, Int) = dict.new()
        seen[Key(1, 10)] = 7
        let found = seen.get_or(Key(1, 99), 0) == 7
        match self:
            Box(left) ->
                match other:
                    Box(right) -> found && left == right

fn main(console: Console):
    console.print("${[Box(1)] == [Box(1)]}")
    console.print("${[Box(1)] == [Box(2)]}")
"#;
    let expected = ["true", "false"].into_iter().map(str::to_owned).collect::<Vec<_>>();
    assert_three_legs(source, expected.clone());
    assert_three_legs(&format!("mode opt\n{source}"), expected);
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
