use witchy::runtime::{Capabilities, Runtime};
use witchy::codegen;

const LIVE_MATCH_BINDING: &str = r#"
import iter

fn allowed(value: Int, console: Console) -> Bool:
    console.print("guard ${value}")
    true

gen fn guarded(console: Console) -> Iter(Int):
    var running: Bool = true
    var current: Option(Int) = Some(7)
    while running:
        match current:
            Some(value) if allowed(value, console) ->
                console.print("before ${value}")
                yield value
                console.print("after ${value}")
                running = false
            _ ->
                running = false

fn drain(values: Iter(Int), console: Console):
    match values.next():
        Empty -> console.print("done")
        Item(value, rest) ->
            console.print("yield ${value}")
            drain(rest, console)

fn main(console: Console):
    drain(guarded(console), console)
"#;

fn compiled_output(checked: &witchy::pipeline::CheckedModule) -> (Vec<String>, Option<usize>) {
    let artifact = codegen::compile_checked_module_artifact(checked)
        .expect_lowered("compile live match-binding generator fixture");
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
        .expect("spawn live match-binding generator fixture");
    actor.run().expect("run live match-binding generator fixture");
    (actor.output(), actor.canonical_heap_leaks().expect("canonical heap audit"))
}

#[test]
fn match_bindings_survive_yield_without_replaying_guards_on_admitted_wasm() {
    let checked = witchy::resolve_std_only_checked(LIVE_MATCH_BINDING)
        .expect("check live match-binding generator fixture");
    let expected = ["guard 7", "before 7", "yield 7", "after 7", "done"];

    assert_eq!(
        compiled_output(&checked),
        (expected.iter().map(|line| (*line).to_string()).collect(), Some(0)),
        "compiled Wasm restores the binding without replaying the guard",
    );
}
