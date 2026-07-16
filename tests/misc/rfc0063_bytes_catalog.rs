//! RFC-0063 Bytes operation catalog parity, including the public `bytes.at`
//! alias that lowers through the canonical private bridge row.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, parser, pipeline, typeck};

#[test]
fn cataloged_bytes_family_matches_on_both_backends() {
    let source = r#"
import bytes

fn main(console: Console):
    let ab = bytes.from_string("AB")
    console.print("${bytes.length(ab)}")
    console.print("${bytes.at(ab, 1)}")
    console.print(bytes.to_string(ab))
    match bytes.from_list([65, 66]):
        Ok(value) -> console.print(bytes.to_string(value))
        Err(_) -> console.print("invalid")
    console.print(bytes.to_string(bytes.slice(bytes.concat(bytes.from_string("A"), bytes.from_string("B")), 1, 2)))
"#;
    let parsed = parser::parse_module(source).expect("parse Bytes catalog program");
    let linked = pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("link Bytes catalog program");
    typeck::check(&linked).expect("typecheck Bytes catalog program");

    let expected = vec!["2", "66", "AB", "AB", "B"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(
        interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpret Bytes catalog program"),
        expected,
    );

    let wasm = codegen::compile_module_binary(&linked).expect_lowered("compile Bytes catalog program");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities { print: true, quiet: true, ..Default::default() },
            64,
        )
        .expect("spawn compiled Bytes catalog program");
    actor.run().expect("run compiled Bytes catalog program");
    assert_eq!(actor.output(), expected);
}
