use super::*;

fn assert_exact_packed_tuple_transport(linked: &ast::Module, relay_suffix: &str) {
    let wir = codegen::assemble_wir_module(linked)
        .expect_lowered("the packed structural tuple lowers to WIR");
    let relay_name = wir
        .funcs
        .iter()
        .map(|function| function.name.as_str())
        .find(|name| name.ends_with(relay_suffix))
        .unwrap_or_else(|| panic!("missing `{relay_suffix}` in lowered WIR"));
    let wat = witchy_wir::wir::to_wat(&wir);

    let relay_signature = wat
        .lines()
        .find(|line| line.starts_with(&format!("  (func ${relay_name} ")))
        .unwrap_or_else(|| panic!("missing `{relay_name}` WAT signature: {wat}"));
    assert_eq!(
        relay_signature,
        format!("  (func ${relay_name} (param $pair i32) (result i32)"),
        "the direct callable transports one exact tuple pointer",
    );
    assert!(wat.contains(&format!("call ${relay_name}")), "tuple crosses `{relay_name}`: {wat}");

    let tuple_helper_signature = wat
        .lines()
        .find(|line| {
            line.starts_with("  (func $__witchy_packed_record_")
                && line.contains("(param $f0 i32) (param $f1 i32) (result i32)")
        })
        .unwrap_or_else(|| panic!("missing exact `(Point, Bool)` tuple helper: {wat}"));
    let tuple_helper = tuple_helper_signature
        .trim_start()
        .strip_prefix("(func $")
        .expect("tuple helper signature prefix")
        .split_whitespace()
        .next()
        .expect("tuple helper name");
    assert!(
        wat.contains(&format!("call ${tuple_helper}")),
        "the tuple constructor uses its canonical descriptor helper: {wat}",
    );
    assert!(!wat.contains("call $mk2"), "no universal two-slot tuple reshape: {wat}");

    let relay_start = wat
        .find(&format!("  (func ${relay_name} "))
        .expect("relay function start");
    let relay_tail = &wat[relay_start..];
    let relay_end = relay_tail[1..]
        .find("\n  (func $")
        .map(|offset| offset + 1)
        .unwrap_or(relay_tail.len());
    let relay = &relay_tail[..relay_end];
    assert!(relay.contains("local.get $pair"), "relay returns the exact incoming pointer: {relay}");
    assert!(!relay.contains("call $"), "relay performs no boxing or reshape: {relay}");
}

#[test]
fn exact_layout_direct_values_and_ownership_backends_agree() {
    let source = r#"
mode opt

type Point packed:
    x: Int
    y: Int

type Token packed:
    Skip
    Value(Int)

fn make() -> Point:
    Point(7, 11)

fn relay(points: List(Point)) -> List(Point):
    points

fn pass(own token: Token) -> Token:
    token

fn score(let token: Token) -> Int:
    match token:
        Skip -> 9
        Value(value) -> value

fn main(console: Console):
    let points = relay([Point(1, 2), Point(3, 4)])
    let answer = make().x * 1000 + list.at(points, 0).x * 100
        + list.length(points) + score(pass(Value(5)))
    console.print("${answer}")
"#;
    let expected = vec!["7107".to_string()];
    assert_eq!(interp(source), expected, "interpreter oracle");
    assert_eq!(run_linked_on_wasm(&[("main", source)], "main"), expected, "Wasm");
}

#[test]
fn exact_packed_structural_tuple_crosses_a_direct_boundary() {
    let source = r#"
mode opt

type Point packed:
    x: Int
    y: Int

fn relay_pair(pair: (Point, Bool)) -> (Point, Bool):
    pair

fn score(pair: (Point, Bool)) -> Int:
    let bonus = if pair.1: 7 else: 3
    pair.0.x * 100 + pair.0.y * 10 + bonus

fn main(console: Console):
    console.print("${score(relay_pair((Point(4, 9), true)))}")
"#;
    let expected = vec!["497".to_string()];
    assert_eq!(interp(source), expected, "independent interpreter oracle");
    assert_eq!(run_linked_on_wasm(&[("main", source)], "main"), expected, "Wasm");

    let module = parser::parse_module(source).expect("parse direct packed tuple");
    let linked = crate::pipeline::link(vec![("main".into(), module)], "main")
        .expect("link direct packed tuple");
    assert_exact_packed_tuple_transport(&linked, "relay_pair");
}

#[test]
fn exact_layout_survives_user_module_linking_on_both_backends() {
    let model = r#"
mode opt

type Point packed:
    x: Int
    y: Int

pub fn relay(points: List(Point)) -> List(Point):
    points

pub fn origin() -> Point:
    Point(5, 8)
"#;
    let app = r#"
mode opt
from model import Point, relay, origin

fn score(point: Point) -> Int:
    point.x * 10 + point.y

fn main(console: Console):
    let points = relay([Point(2, 3), Point(7, 11)])
    let answer = score(origin()) * 100 + list.at(points, 1).x * 10
        + list.at(points, 0).y
    console.print("${answer}")
"#;
    let modules = [
        ("model".to_string(), parser::parse_module(model).expect("parse model")),
        ("app".to_string(), parser::parse_module(app).expect("parse app")),
    ];
    let linked = crate::pipeline::link(modules.to_vec(), "app").expect("link interpreter modules");
    let interpreted = interpreter::run_module(linked, ".", Vec::new()).expect("interpreter");
    let expected = vec!["5873".to_string()];
    assert_eq!(interpreted, expected, "interpreter oracle");
    assert_eq!(
        run_linked_on_wasm(&[("model", model), ("app", app)], "app"),
        expected,
        "Wasm",
    );
}

#[test]
fn exact_packed_structural_tuple_survives_user_module_linking() {
    let model = r#"
mode opt

type Point packed:
    x: Int
    y: Int

pub fn relay_pair(pair: (Point, Bool)) -> (Point, Bool):
    pair

pub fn origin_pair() -> (Point, Bool):
    (Point(6, 8), false)
"#;
    let app = r#"
mode opt
from model import Point, relay_pair, origin_pair

fn score(pair: (Point, Bool)) -> Int:
    let bonus = if pair.1: 1 else: 0
    pair.0.x * 100 + pair.0.y * 10 + bonus

fn main(console: Console):
    let relayed = relay_pair((Point(2, 5), true))
    console.print("${score(relayed) * 1000 + score(origin_pair())}")
"#;
    let modules = [
        ("model".to_string(), parser::parse_module(model).expect("parse tuple model")),
        ("app".to_string(), parser::parse_module(app).expect("parse tuple app")),
    ];
    let linked = crate::pipeline::link(modules.to_vec(), "app").expect("link tuple modules");
    let interpreted = interpreter::run_module(linked.clone(), ".", Vec::new())
        .expect("interpreter");
    let expected = vec!["251680".to_string()];
    assert_eq!(interpreted, expected, "independent interpreter oracle");
    assert_eq!(
        run_linked_on_wasm(&[("model", model), ("app", app)], "app"),
        expected,
        "Wasm",
    );
    assert_exact_packed_tuple_transport(&linked, "relay_pair");
}

#[test]
fn exact_packed_list_stream_backends_agree() {
    let source = r#"
mode opt
import list

type Point packed:
    x: Int
    y: Int

fn main(console: Console):
    var points = []
    for i in 0..9:
        list.push(points, Point(i, i * 3))
    var total = 0
    for point in points:
        total = total + point.x * 7 + point.y
    console.print("${total * 100 + list.length(points)}")
"#;
    let expected = vec!["36009".to_string()];
    assert_eq!(interp(source), expected, "interpreter oracle");
    assert_eq!(run_linked_on_wasm(&[("main", source)], "main"), expected, "Wasm");
}
