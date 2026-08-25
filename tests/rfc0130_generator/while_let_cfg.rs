use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};

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
    witchy_interp::compiler_natives::install();
    let bytes = codegen::compile_checked_module_binary(checked)
        .expect_lowered("compile generator while-let CFG fixture");
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
        .expect("spawn generator while-let CFG fixture");
    actor.run().expect("run generator while-let CFG fixture");
    actor.output()
}

#[test]
fn while_let_bindings_and_effects_resume_once_on_both_backends() {
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

    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("interpret generator while-let CFG fixture");
    assert_eq!(interpreted, expected, "interpreter preserves while-let suspension state");
    assert_eq!(
        compiled_output(&checked),
        expected,
        "compiled Wasm preserves the live binding and one-time effects",
    );
}
