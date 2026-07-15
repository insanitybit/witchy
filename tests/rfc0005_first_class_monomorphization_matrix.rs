//! Deterministic adversarial matrix for RFC-0005 first-class monomorphization.
//! The interpreter is the oracle for optimized and unoptimized compiled WASM.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, opt, parser, pipeline, typeck};

struct OptOverride;

impl OptOverride {
    fn set(options: opt::OptSet) -> Self {
        opt::set_for_tests(Some(options));
        Self
    }
}

impl Drop for OptOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

#[test]
fn adversarial_first_class_monomorphization_matches_both_backends() {
    let source = r#"
trait Numbered:
    fn number(self) -> Int

type NumberBox:
    NumberBox(Int)

impl Numbered for NumberBox:
    fn number(self) -> Int:
        match self:
            NumberBox(value) -> value

fn id(value: a) -> a:
    value

fn forward1() -> fn(a) -> a:
    id

fn forward2() -> fn(a) -> a:
    forward1()

fn forward3() -> fn(a) -> a:
    forward2()

fn forward4() -> fn(a) -> a:
    forward3()

fn forward5() -> fn(a) -> a:
    forward4()

fn forward6() -> fn(a) -> a:
    forward5()

fn forward7() -> fn(a) -> a:
    forward6()

fn forward8() -> fn(a) -> a:
    forward7()

fn forward9() -> fn(a) -> a:
    forward8()

fn forward10() -> fn(a) -> a:
    forward9()

fn forward11() -> fn(a) -> a:
    forward10()

fn forward12() -> fn(a) -> a:
    forward11()

fn forward13() -> fn(a) -> a:
    forward12()

fn forward14() -> fn(a) -> a:
    forward13()

fn forward15() -> fn(a) -> a:
    forward14()

fn forward16() -> fn(a) -> a:
    forward15()

fn nested(left: a, right: b) -> (Option(a), List(b)):
    (Some(left), [right])

fn make_none() -> Option(a):
    None

fn read_number(value: a) -> Int where a: Numbered:
    value.number()

fn pass_function(f: fn(a) -> b) -> fn(a) -> b:
    f

fn bounded0() -> fn(a) -> Int:
    read_number

fn bounded1() -> fn(a) -> Int:
    pass_function(bounded0())

fn bounded2() -> fn(a) -> Int:
    pass_function(bounded1())

fn second(ignore, value: a) -> a:
    value

fn replace(var current: a, next: a) -> a:
    current = next
    current

fn shadow_expression() -> Int:
    let id = fn(value: Int) -> Int: value + 1
    (id)(5000000010)

fn local_call_syntaxes() -> (Int, Int):
    let callable = fn(value: Int) -> Int: value + 1
    (callable(5000000011), (callable)(5000000012))

fn main(console: Console):
    let depth1 = forward1()
    console.print(if depth1(5000000000) == 5000000000: "depth-1-int" else: "depth-1-failed")

    let depth5 = forward5()
    console.print(if depth5("five") == "five": "depth-5-string" else: "depth-5-failed")

    let depth9 = forward9()
    console.print(if depth9(true): "depth-9-bool" else: "depth-9-failed")

    let depth16 = forward16()
    console.print(if depth16(16.25) == 16.25: "depth-16-float" else: "depth-16-failed")

    let scalar_id = id
    console.print(if scalar_id(5000000001) == 5000000001: "big-param-result" else: "big-param-result-failed")

    let tuple_id = id
    let (tuple_big, tuple_tag) = tuple_id((5000000002, "tuple"))
    console.print(if tuple_big == 5000000002 && tuple_tag == "tuple": "big-tuple" else: "big-tuple-failed")

    let list_id = id
    let big_list = list_id([5000000003])
    console.print(if list.at(big_list, 0) == 5000000003: "big-list" else: "big-list-failed")

    let option_id = id
    match option_id(Some(5000000004)):
        Some(value) -> console.print(if value == 5000000004: "big-option" else: "big-option-failed")
        None -> console.print("big-option-failed")

    var assigned = fn(value: Int) -> Int: value
    assigned = id
    console.print(if assigned(5000000005) == 5000000005: "big-assignment" else: "big-assignment-failed")

    let joined = if true: id else: id
    console.print(if joined(5000000006) == 5000000006: "big-join" else: "big-join-failed")

    let nested_value = nested
    let (nested_option, nested_list) = nested_value("nested", 5000000007)
    match nested_option:
        Some(value) -> console.print(if value == "nested" && list.at(nested_list, 0) == 5000000007: "multi-nested" else: "multi-nested-failed")
        None -> console.print("multi-nested-failed")

    let missing_int: fn() -> Option(Int) = make_none
    match missing_int():
        Some(_value) -> console.print("result-only-failed")
        None -> console.print("result-only")

    let bounded = bounded2()
    console.print(if bounded(NumberBox(73)) == 73: "bounded-return" else: "bounded-return-failed")

    let second_value = second
    console.print(if second_value(false, 5000000008) == 5000000008: "unannotated-param" else: "unannotated-param-failed")

    let replace_value = replace
    var current = 0
    let returned = replace_value(current, 5000000009)
    console.print(if current == 5000000009 && returned == 5000000009: "var-writeback" else: "var-writeback-failed")

    console.print(if shadow_expression() == 5000000011: "shadow-expression" else: "shadow-expression-failed")

    let (ordinary, explicit) = local_call_syntaxes()
    console.print(if ordinary == 5000000012 && explicit == 5000000013: "ordinary-explicit" else: "ordinary-explicit-failed")

    console.print(if (forward1())(5000000014) == 5000000014: "explicit-generic" else: "explicit-generic-failed")
"#;
    let module = parser::parse_module(source).expect("parse matrix");
    let linked = pipeline::link(vec![("main".into(), module)], "main").expect("link matrix");
    typeck::check(&linked).expect("first-class monomorphization matrix typechecks");

    let expected = [
        "depth-1-int",
        "depth-5-string",
        "depth-9-bool",
        "depth-16-float",
        "big-param-result",
        "big-tuple",
        "big-list",
        "big-option",
        "big-assignment",
        "big-join",
        "multi-nested",
        "result-only",
        "bounded-return",
        "unannotated-param",
        "var-writeback",
        "shadow-expression",
        "ordinary-explicit",
        "explicit-generic",
    ]
    .map(str::to_string)
    .to_vec();

    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret matrix"),
        expected,
        "interpreter first-class monomorphization matrix",
    );

    let configurations = [("optimized", opt::OptSet::all()), ("unoptimized", opt::OptSet::none())];
    for (configuration, options) in configurations {
        let _opt_override = OptOverride::set(options);
        let wasm = codegen::compile_module_binary(&linked)
            .expect("compile matrix")
            .unwrap_or_else(|| panic!("{configuration}: matrix lowers to WIR"));
        let mut runtime = Runtime::batch().expect("runtime");
        let mut actor = runtime
            .spawn(
                &wasm,
                Capabilities {
                    print: true,
                    quiet: true,
                    ..Default::default()
                },
                64,
            )
            .expect("spawn compiled matrix");
        actor.run().expect("run compiled matrix");
        assert_eq!(
            actor.output(),
            expected,
            "{configuration}: compiled first-class monomorphization matrix",
        );
    }
}
