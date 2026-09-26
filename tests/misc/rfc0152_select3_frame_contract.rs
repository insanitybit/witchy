//! RFC-0152 fixed-arity three-way select: source-order readiness and the
//! final closed wake after all queued messages are consumed.

use witchy::runtime::{Capabilities, Runtime};
use witchy::codegen;

// This fixture exercises a payload-free receive body.
// The channel adapter is normal-mode; opt awaits require an opt-compatible
// task producer under RFC-0167.
const RECV_READY_FRAME_CONTRACT: &str = r#"
async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    chan.send(tx, 3).await
    chan.send(tx, 5).await
    chan.send(tx, 7).await
    chan.send(tx, 11).await

    var seen = 0
    for await _value in rx:
        seen = seen + 1
    console.print("seen=${seen}")
"#;

/// Run the program the way the product does: an admitted canonical artifact,
/// not a raw Wasm binary. The canonical route refuses a raw module ("raw Wasm
/// cannot claim canonical admission"), which is why every test through this
/// helper had stopped running.
fn run_compiled(source: &str) -> Vec<String> {
    let checked = witchy::resolve_std_only_checked(source)
        .expect("RFC-0152 select3 frame contract must check");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("compile RFC-0152 select3 frame contract");
    let mut runtime = Runtime::batch().expect("create RFC-0152 select3 frame runtime");
    let mut actor = runtime
        .spawn_admitted(
            &artifact,
            Capabilities {
                print: true,
                quiet: true,
                ..Default::default()
            },
            256,
        )
        .expect("spawn RFC-0152 select3 frame contract");
    actor.run().expect("run RFC-0152 select3 frame contract");
    assert_eq!(actor.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
    actor.output()
}

#[test]
fn rfc0152_payload_free_receive_runs_on_the_admitted_baseline() {
    // Four queued messages are consumed before the closed wake ends the loop.
    assert_eq!(run_compiled(RECV_READY_FRAME_CONTRACT), ["seen=4"]);
}

/// A `select` whose match is a VALUE, inside a loop, links and runs.
///
/// Arms that only act take the direct-result path; a match feeding a binding
/// makes the desugar emit `chan.selectN_frame_payload`/`_decode` into the
/// user's own module. The scheduler-bridge privacy rule — which exists to stop
/// SOURCE from forging scheduler authority — was rejecting that compiler
/// output, so the shape did not link at all (BUG-639). This runs in the
/// default gate: a documented feature failing to compile is not nightly news.
const SELECT_VALUE_MATCH_IN_A_LOOP: &str = r#"
async fn main(console: Console):
    let (a_tx, a_rx) = chan.channel(2).await
    let (b_tx, b_rx) = chan.channel(2).await
    chan.send(a_tx, 1).await
    chan.send(b_tx, 100).await
    var seen = 0
    var total = 0
    while seen < 2:
        let selected = chan.select(a_rx, b_rx).await
        let (got, value) = match selected:
            First(x) -> (1, x)
            Second(y) -> (1, y)
            Closed -> (0, 0)
        total = total + value
        seen = seen + got
    console.print("total=${total}")
"#;

#[test]
fn a_select_match_bound_to_a_value_links_and_runs_inside_a_loop() {
    // The independent expected result: both buffered values are taken once,
    // so the loop ends having added 1 and 100.
    assert_eq!(run_compiled(SELECT_VALUE_MATCH_IN_A_LOOP), ["total=101"]);
}
