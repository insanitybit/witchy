use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};

const TWO_SUSPENDING_BRANCHES: &str = r#"
import iter

gen fn alternating(console: Console) -> Iter(Int):
    var i: Int = 0
    while i < 4:
        console.print("scan ${i}")
        if i % 2 == 0:
            console.print("even before ${i}")
            yield i
            console.print("even after ${i}")
        else:
            console.print("odd before ${i}")
            yield i + 10
            console.print("odd after ${i}")
        i = i + 1

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
        .expect_lowered("compile nested generator CFG fixture");
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
        .expect("spawn nested generator CFG fixture");
    actor.run().expect("run nested generator CFG fixture");
    actor.output()
}

#[test]
fn two_suspending_branches_resume_once_on_both_backends() {
    let checked = witchy::resolve_std_only_checked(TWO_SUSPENDING_BRANCHES)
        .expect("check nested generator CFG fixture");
    let expected = [
        "scan 0",
        "even before 0",
        "yield 0",
        "even after 0",
        "scan 1",
        "odd before 1",
        "yield 11",
        "odd after 1",
        "scan 2",
        "even before 2",
        "yield 2",
        "even after 2",
        "scan 3",
        "odd before 3",
        "yield 13",
        "odd after 3",
        "done",
    ];

    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("interpret nested generator CFG fixture");
    assert_eq!(interpreted, expected, "interpreter must resume the selected branch");
    assert_eq!(
        compiled_output(&checked),
        expected,
        "compiled Wasm must preserve branch identity and one-time effects",
    );
}
