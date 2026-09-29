use super::*;

    #[test]
    fn order_processing_integration_backends_agree() {
        let client = r#"
import list
import option

type Item:
    name: String
    price: Int
    qty: Int

fn line_total(it: Item) -> Int:
    ((it).price * (it).qty)

fn main(console: Console):
    let cart = [Item("apple", 50, 3), Item("bread", 200, 1), Item("milk", 150, 2)]
    let total = list.fold(cart, 0, fn(acc: Int, it: Item): (acc + line_total(it)))
    console.print("${total}")
    match list.max_by(cart, fn(a: Item, b: Item): (line_total(a) < line_total(b))):
        Some(it) -> console.print((it).name)
        None -> console.print("none")
"#;
        let sources = [("main", client)];
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(compiled, vec!["650", "milk"]);
    }

    // Nested constructor patterns destructure through a record: `Circle(Point(x,
    // y))` binds x and y from the inner Point in one pattern.
    #[test]
    fn nested_constructor_pattern_backends_agree() {
        let src = r#"
type Point:
    x: Int
    y: Int

type Shape:
    Circle(Point)
    Origin

fn f(s: Shape) -> Int:
    match s:
        Circle(Point(x, y)) -> (x + y)
        Origin -> 0

fn main(console: Console):
    console.print("${f(Circle(Point(3, 4)))}")
    console.print("${f(Circle(Point(10, 1)))}")
    console.print("${f(Origin)}")
"#;
        assert_eq!(run_on_wasm(src), vec!["7", "11", "0"]);
    }
