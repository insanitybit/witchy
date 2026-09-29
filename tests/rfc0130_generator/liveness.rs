use witchy::runtime::{Capabilities, Runtime};
use witchy::codegen;

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
    let artifact = codegen::compile_checked_module_artifact(checked)
        .expect_lowered("compile generator loop-local liveness fixture");
    let mut runtime = Runtime::batch_quick().expect("create runtime");
    let mut actor = runtime
        .spawn_admitted(
            &artifact,
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
fn loop_local_live_after_yield_is_carried_without_replay_on_admitted_wasm() {
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

    assert_eq!(
        compiled_output(&checked),
        expected,
        "compiled Wasm must carry the loop local and preserve effect order",
    );
}
