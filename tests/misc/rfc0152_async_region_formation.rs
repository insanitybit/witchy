//! RFC-0152 Phase 1: effects outside an eligible channel region remain normal
//! source-order work. Unsupported awaits are covered by the non-executing
//! lowered-carrier test, because this executable oracle must not depend on an
//! unsupported task implementation.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};

const REGION_BOUNDARY_CONTRACT: &str = r#"
async fn main(console: Console):
    console.print("before")
    let (tx, rx) = chan.channel(1).await

    // This ordinary effect is intentionally outside the ready region. The
    // carrier split test covers an unsupported awaited interior region; this
    // executable oracle proves that ordinary effects on both sides remain in
    // source order on interpreter and both Wasm modes.
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
    let wasm = codegen::compile_checked_module_binary(&checked)
        .expect_lowered("compile RFC-0152 region-boundary fixture");
    let mut runtime = Runtime::batch().expect("create RFC-0152 region-boundary runtime");
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
        .expect("spawn RFC-0152 region-boundary fixture");
    actor.run().expect("run RFC-0152 region-boundary fixture");
    actor.output()
}

fn interpreted_output(source: &str) -> Vec<String> {
    let checked = witchy::resolve_std_only_checked(source)
        .expect("RFC-0152 region-boundary fixture must check");
    interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("interpret RFC-0152 region-boundary fixture")
}

#[test]
fn rfc0152_region_sources_remain_accepted_in_both_modes() {
    let ordinary = [
        REGION_BOUNDARY_CONTRACT.to_owned(),
        guarded_boundary_contract(true),
        guarded_boundary_contract(false),
    ];
    for source in ordinary {
        for candidate in [source.clone(), format!("mode opt\n\n{source}")] {
            witchy::resolve_std_only_checked(&candidate)
                .expect("migrated RFC-0152 region source must check in both modes");
        }
    }
}

#[test]
fn rfc0152_region_boundaries_preserve_effect_order_and_backend_parity() {
    let full_matrix = std::env::var_os("WITCHY_FULL_DIFFERENTIAL_MATRIX").is_some_and(|v| v == "1")
        || std::env::var_os("WITCHY_NIGHTLY").is_some_and(|v| v == "1");
    let expected = ["before", "middle=2", "value=Some(7)", "after"];
    let ordinary_interpreted = interpreted_output(REGION_BOUNDARY_CONTRACT);
    assert_eq!(ordinary_interpreted, expected, "independent source-order result");

    let optimized = format!("mode opt\n\n{REGION_BOUNDARY_CONTRACT}");
    let optimized_compiled = compiled_output(&optimized);
    assert_eq!(optimized_compiled, ordinary_interpreted, "optimized Wasm parity with oracle");

    if full_matrix {
        let ordinary_compiled = compiled_output(REGION_BOUNDARY_CONTRACT);
        assert_eq!(ordinary_compiled, ordinary_interpreted, "ordinary Wasm parity");
        let optimized_interpreted = interpreted_output(&optimized);
        assert_eq!(optimized_interpreted, expected, "optimized interpreter result");
        assert_eq!(optimized_compiled, optimized_interpreted, "optimized Wasm parity");
        assert_eq!(optimized_compiled, ordinary_compiled, "region formation does not move effects");
    }
}

#[test]
fn rfc0152_guarded_paths_preserve_both_outcomes_and_backend_parity() {
    let full_matrix = std::env::var_os("WITCHY_FULL_DIFFERENTIAL_MATRIX").is_some_and(|v| v == "1")
        || std::env::var_os("WITCHY_NIGHTLY").is_some_and(|v| v == "1");
    for (take_send, expected) in [
        (true, vec!["before", "sent", "after"]),
        (false, vec!["before", "skipped", "after"]),
    ] {
        let ordinary = guarded_boundary_contract(take_send);
        let optimized = format!("mode opt\n\n{ordinary}");
        let interpreted = interpreted_output(&ordinary);
        let compiled = compiled_output(&optimized);
        assert_eq!(interpreted, expected, "interpreter outcome for take_send={take_send}");
        assert_eq!(compiled, interpreted, "Wasm outcome for take_send={take_send}");

        if full_matrix {
            let ordinary_compiled = compiled_output(&ordinary);
            let opt_interpreted = interpreted_output(&optimized);
            assert_eq!(ordinary_compiled, interpreted, "ordinary Wasm matches interpreter");
            assert_eq!(opt_interpreted, interpreted, "opt interpreter matches unopt interpreter");
        }
    }
}
