use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};

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

fn compiled_output(checked: &witchy::pipeline::CheckedModule) -> Vec<String> {
    witchy_interp::compiler_natives::install();
    let bytes = codegen::compile_checked_module_binary(checked)
        .expect_lowered("compile live match-binding generator fixture");
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
        .expect("spawn live match-binding generator fixture");
    actor.run().expect("run live match-binding generator fixture");
    actor.output()
}

#[test]
fn match_bindings_survive_yield_without_replaying_guards_on_both_backends() {
    let checked = witchy::resolve_std_only_checked(LIVE_MATCH_BINDING)
        .expect("check live match-binding generator fixture");
    let expected = ["guard 7", "before 7", "yield 7", "after 7", "done"];

    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("interpret live match-binding generator fixture");
    assert_eq!(interpreted, expected, "interpreter resumes with the selected binding");
    assert_eq!(
        compiled_output(&checked),
        expected,
        "compiled Wasm restores the binding without replaying the guard",
    );
}
