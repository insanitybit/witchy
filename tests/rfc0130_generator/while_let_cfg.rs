use witchy::runtime::{Capabilities, Runtime};
use witchy::codegen;

const SUSPENDING_WHILE_LET: &str = r#"
import iter

gen fn staged(console: Console) -> Iter(Int):
    var current: Option(Int) = Some(0)
    while let Some(value) = current:
        console.print("before ${value}")
        yield value
        console.print("after ${value}")
        current = if value < 2: Some(value + 1) else: None

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
        .expect_lowered("compile generator while-let CFG fixture");
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
        .expect("spawn generator while-let CFG fixture");
    actor.run().expect("run generator while-let CFG fixture");
    actor.output()
}

#[test]
fn while_let_bindings_and_effects_resume_once_on_admitted_wasm() {
    let checked = witchy::resolve_std_only_checked(SUSPENDING_WHILE_LET)
        .expect("check generator while-let CFG fixture");
    let expected = [
        "before 0",
        "yield 0",
        "after 0",
        "before 1",
        "yield 1",
        "after 1",
        "before 2",
        "yield 2",
        "after 2",
        "done",
    ];

    assert_eq!(
        compiled_output(&checked),
        expected,
        "compiled Wasm preserves the live binding and one-time effects",
    );
}
