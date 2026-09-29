//! RFC-0152 Phase 0: a compact normal-mode channel schedule matrix with
//! independent expected results on admitted Wasm. The cases cover distinct
//! scheduler boundaries.

use witchy::runtime::{Capabilities, Runtime};
use witchy::codegen;

struct ChannelCase {
    label: &'static str,
    source: &'static str,
    expected: &'static [&'static str],
}

const CASES: &[ChannelCase] = &[
    ChannelCase {
        label: "ready select4 source order",
        source: r#"
async fn main(console: Console):
    let (a_tx, a_rx) = chan.channel(8).await
    let (b_tx, b_rx) = chan.channel(8).await
    let (c_tx, c_rx) = chan.channel(8).await
    let (d_tx, d_rx) = chan.channel(8).await
    chan.send(c_tx, 30).await
    chan.send(d_tx, 40).await
    let selected = chan.select4(a_rx, b_rx, c_rx, d_rx).await
    match selected:
        First4(value) -> console.print("wrong first ${value}")
        Second4(value) -> console.print("wrong second ${value}")
        Third4(value) -> console.print("ready ${value}")
        Fourth4(value) -> console.print("wrong fourth ${value}")
        Closed4 -> console.print("closed too early")
"#,
        expected: &["ready 30"],
    },
];

fn run_compiled(source: &str, label: &str) -> Vec<String> {
    let checked = witchy::resolve_std_only_checked(source)
        .unwrap_or_else(|error| panic!("{label}: source must check: {error}"));
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered(&format!("{label}: admit channel schedule"));
    let mut runtime = Runtime::batch().unwrap_or_else(|error| panic!("{label}: create runtime: {error}"));
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
        .unwrap_or_else(|error| panic!("{label}: spawn compiled Wasm: {error}"));
    actor.run().unwrap_or_else(|error| panic!("{label}: run compiled Wasm: {error}"));
    actor.output()
}

fn assert_case(case: &ChannelCase) {
    let actual = run_compiled(case.source, case.label);
    assert_eq!(actual, case.expected, "{}: independent expected result", case.label);
}

#[test]
fn rfc0152_phase0_ready_select4_matrix() {
    assert_case(&CASES[0]);
}
