use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};

const SUSPENDING_MATCH_ARMS: &str = r#"
import iter

gen fn alternating(console: Console) -> Iter(Int):
    var i: Int = 0
    var current: Option(Int) = Some(0)
    while i < 4:
        console.print("scan ${i}")
        match current:
            Some(value) ->
                console.print("some before")
                yield value
                console.print("some after")
                i = i + 1
                current = None
            None ->
                console.print("none before")
                yield i + 10
                console.print("none after")
                i = i + 1
                current = Some(i)

fn drain(values: Iter(Int), console: Console):
    match values.next():
        Empty -> console.print("done")
        Item(value, rest) ->
            console.print("yield ${value}")
            drain(rest, console)

fn main(console: Console):
    drain(alternating(console), console)
"#;

fn compiled_output(checked: &witchy::pipeline::CheckedModule) -> Vec<String> {
    witchy_interp::compiler_natives::install();
    let bytes = codegen::compile_checked_module_binary(checked)
        .expect_lowered("compile generator match-CFG fixture");
    let mut runtime = Runtime::batch_quick().expect("create runtime");
    let mut actor = runtime
        .spawn(
            &bytes,
            Capabilities {
                print: true,
                print_int: true,
                quiet: true,
                ..Default::default()
            },
            64,
        )
        .expect("spawn generator match-CFG fixture");
    actor.run().expect("run generator match-CFG fixture");
    actor.output()
}

#[test]
fn suspending_match_arms_resume_once_on_both_backends() {
    let checked = witchy::resolve_std_only_checked(SUSPENDING_MATCH_ARMS)
        .expect("check generator match-CFG fixture");
    let expected = [
        "scan 0",
        "some before",
        "yield 0",
        "some after",
        "scan 1",
        "none before",
        "yield 11",
        "none after",
        "scan 2",
        "some before",
        "yield 2",
        "some after",
        "scan 3",
        "none before",
        "yield 13",
        "none after",
        "done",
    ];

    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("interpret generator match-CFG fixture");
    assert_eq!(interpreted, expected, "interpreter must resume the selected match arm");
    assert_eq!(
        compiled_output(&checked),
        expected,
        "compiled Wasm must preserve match-arm identity and one-time effects",
    );
}
