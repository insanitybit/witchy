//! RFC-0152 fixed-arity three-way select: source-order readiness and the
//! compiler-owned frame fallback must agree with the interpreter, including
//! the final closed wake after all queued messages are consumed.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};

const SELECT3_FRAME_CONTRACT: &str = r#"
mode opt

async fn main(console: Console):
    let (a_tx, a_rx) = chan.channel(2).await
    let (b_tx, b_rx) = chan.channel(2).await
    let (c_tx, c_rx) = chan.channel(2).await
    chan.send(a_tx, 1).await
    chan.send(a_tx, 2).await
    chan.send(b_tx, 10).await
    chan.send(b_tx, 20).await
    chan.send(c_tx, 100).await
    chan.send(c_tx, 200).await

    var seen = 0
    var sum = 0
    while seen < 7:
        let selected = chan.select3(a_rx, b_rx, c_rx).await
        match selected:
            First3(value) ->
                sum = sum + value
                seen = seen + 1
            Second3(value) ->
                sum = sum + value
                seen = seen + 1
            Third3(value) ->
                sum = sum + value
                seen = seen + 1
            Closed3 -> seen = 7
    console.print("seen=${seen} sum=${sum}")
"#;

fn run_compiled(source: &str) -> Vec<String> {
    let checked = witchy::resolve_std_only_checked(source)
        .expect("RFC-0152 select3 frame contract must check");
    let wasm = codegen::compile_checked_module_binary(&checked)
        .expect_lowered("compile RFC-0152 select3 frame contract");
    let mut runtime = Runtime::batch().expect("create RFC-0152 select3 frame runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities {
                print: true,
                quiet: true,
                ..Default::default()
            },
            256,
        )
        .expect("spawn RFC-0152 select3 frame contract");
    actor.run().expect("run RFC-0152 select3 frame contract");
    actor.output()
}

#[test]
fn rfc0152_select3_frame_preserves_source_order_and_closed_wake_on_both_backends() {
    let checked = witchy::resolve_std_only_checked(SELECT3_FRAME_CONTRACT)
        .expect("RFC-0152 select3 frame contract must check");
    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("run RFC-0152 select3 frame contract on the interpreter");
    let compiled = run_compiled(SELECT3_FRAME_CONTRACT);

    assert_eq!(compiled, interpreted, "three-way frame scheduling must retain interpreter semantics");
    assert_eq!(interpreted, ["seen=7 sum=333"]);
}
