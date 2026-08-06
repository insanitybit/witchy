//! RFC-0050 regression for moving std implementation ownership from a module
//! function to an inherent method without breaking module-call compatibility.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, parser, pipeline, typeck};

#[test]
fn list_filter_method_owns_body_and_module_aliases_stay_compatible() {
    let source = r#"import list

fn even(n: Int) -> Bool:
    n % 2 == 0

fn above_two(n: Int) -> Bool:
    n > 2

fn render(xs: List(Int)) -> String:
    list.join(list.map(xs, fn(n: Int): "${n}"), ",")

fn main(console: Console):
    let xs = [1, 2, 3, 4]
    console.print(render(xs.filter(even)))
    console.print(render(list.filter(xs, even)))
    let filter = list.filter
    console.print(render(filter(xs, above_two)))
"#;
    let module = parser::parse_module(source).expect("parse");
    let linked = pipeline::link(vec![("main".into(), module)], "main").expect("link");
    typeck::check(&linked).expect("typecheck");

    let expected = vec!["2,4".to_string(), "2,4".to_string(), "3,4".to_string()];
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", vec![]).expect("interpret"),
        expected,
        "interpreter filter surfaces",
    );

    let wir = codegen::assemble_wir_module(&linked)

        .expect_lowered("program supports compiled execution");
    let names: Vec<&str> = wir
        .funcs
        .iter()
        .map(|function| function.name.as_str())
        .collect();
    assert!(
        names.iter().any(|name| name.starts_with("List__filter")),
        "filter must compile through its inherent method body: {names:?}",
    );
    assert!(
        !names.iter().any(|name| name.starts_with("list.filter")),
        "legacy module wrapper must not be emitted: {names:?}",
    );

    let wasm = codegen::compile_module_binary(&linked)

        .expect_lowered("program supports compiled execution");
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
        .expect("spawn");
    actor.run().expect("compiled execution");
    assert_eq!(actor.output(), expected, "compiled filter surfaces");
}
