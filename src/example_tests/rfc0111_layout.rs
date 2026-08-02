use super::*;

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
