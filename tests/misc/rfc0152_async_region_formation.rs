//! RFC-0152 Phase 1: effects outside an eligible channel region remain normal
//! source-order work on admitted normal-mode Wasm.

use witchy::runtime::{Capabilities, Runtime};
use witchy::codegen;

const REGION_BOUNDARY_CONTRACT: &str = r#"
async fn main(console: Console):
    console.print("before")
    let (tx, rx) = chan.channel(1).await

    // This ordinary effect is intentionally outside the ready region. The
    // carrier split test covers an unsupported awaited interior region; this
    // executable oracle proves that ordinary effects on both sides remain in
    // source order.
    console.print("middle=2")

    chan.send(tx, 7).await
    let value = chan.recv(rx).await
    console.print("value=${value}")
    console.print("after")
"#;

fn guarded_boundary_contract(take_send: bool) -> String {
    format!(
        r#"
import task

async fn main(console: Console):
    console.print("before")
    let work = if {take_send}:
        task.done("sent")
    else:
        task.done("skipped")
    let outcome = work.await
    console.print(outcome)
    console.print("after")
"#
    )
}

fn compiled_output(source: &str) -> Vec<String> {
    let checked = witchy::resolve_std_only_checked(source)
        .expect("RFC-0152 region-boundary fixture must check");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("compile RFC-0152 region-boundary fixture");
    let mut runtime = Runtime::batch().expect("create RFC-0152 region-boundary runtime");
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
        .expect("spawn RFC-0152 region-boundary fixture");
    actor.run().expect("run RFC-0152 region-boundary fixture");
    actor.output()
}

#[test]
fn rfc0152_region_boundaries_preserve_effect_order_on_admitted_wasm() {
    let expected = ["before", "middle=2", "value=Some(7)", "after"];
    assert_eq!(compiled_output(REGION_BOUNDARY_CONTRACT), expected, "independent source-order result");
}

#[test]
fn rfc0152_guarded_paths_preserve_both_outcomes_on_admitted_wasm() {
    for (take_send, expected) in [
        (true, vec!["before", "sent", "after"]),
        (false, vec!["before", "skipped", "after"]),
    ] {
        let source = guarded_boundary_contract(take_send);
        assert_eq!(compiled_output(&source), expected, "guarded outcome for take_send={take_send}");
    }
}
