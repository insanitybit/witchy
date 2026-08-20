use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};

const LOOP_LOCAL_AFTER_YIELD: &str = r#"
import iter

gen fn staged(console: Console) -> Iter(Int):
    var i: Int = 0
    while i < 3:
        let snapshot: Int = i * 10 + 1
        console.print("before ${snapshot}")
        yield snapshot
        console.print("after ${snapshot}")
        i = i + 1

fn drain(values: Iter(Int), console: Console):
    match values.next():
        Empty -> console.print("done")
        Item(value, rest) ->
            console.print("yield ${value}")
            drain(rest, console)

fn main(console: Console):
    drain(staged(console), console)
"#;

fn compiled_output(checked: &witchy::pipeline::CheckedModule) -> Vec<String> {
    witchy_interp::compiler_natives::install();
    let bytes = codegen::compile_checked_module_binary(checked)
        .expect_lowered("compile generator loop-local liveness fixture");
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
        .expect("spawn generator loop-local liveness fixture");
    actor.run().expect("run generator loop-local liveness fixture");
    actor.output()
}

#[test]
fn loop_local_live_after_yield_is_carried_without_replay_on_both_backends() {
    let checked = witchy::resolve_std_only_checked(LOOP_LOCAL_AFTER_YIELD)
        .expect("check generator loop-local liveness fixture");
    let expected = [
        "before 1",
        "yield 1",
        "after 1",
        "before 11",
        "yield 11",
        "after 11",
        "before 21",
        "yield 21",
        "after 21",
        "done",
    ];

    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("interpret generator loop-local liveness fixture");
    assert_eq!(interpreted, expected, "interpreter must resume after each yield");
    assert_eq!(
        compiled_output(&checked),
        expected,
        "compiled Wasm must carry the loop local and preserve effect order",
    );
}
