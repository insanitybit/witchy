//! RFC-0122 measured checker/runtime telemetry.

use witchy::stats;
use witchy_syntax::opt::{self, Opt, OptSet};

const FIXTURE: &str = include_str!("rfc0122/telemetry_reference_list.witchy");
const RETURN_FIXTURE: &str = include_str!("rfc0122/telemetry_reference_return.witchy");
const PRECISION_ARTIFACT: &str = include_str!("rfc0122/telemetry_precision.expected");

#[test]
fn aggregate_reference_timing_reports_checker_and_execution_samples() {
    opt::set_for_tests(Some(OptSet::default_set()));
    let optimized = stats::compute_timed(FIXTURE).expect("measure optimized RFC-0122 fixture");
    opt::set_for_tests(Some(OptSet::default_set().without(Opt::InPlace)));
    let forced_copy = stats::compute_timed(FIXTURE).expect("measure forced-copy RFC-0122 fixture");
    opt::set_for_tests(None);

    assert_eq!(optimized.stats.output, ["resumed resumed"]);
    assert_eq!(forced_copy.stats.output, optimized.stats.output);
    assert!(optimized.checker_time_us > 0);
    assert!(optimized.execution_time_us > 0);
    assert!(forced_copy.checker_time_us > 0);
    assert!(forced_copy.execution_time_us > 0);
    assert_eq!(
        optimized.stats.loan_control_flow_edges,
        forced_copy.stats.loan_control_flow_edges,
        "timed optimized and forced-copy runs must share the checked graph"
    );
}

#[test]
fn precision_telemetry_artifact_covers_each_reference_phase() {
    assert!(PRECISION_ARTIFACT.contains(
        "schema=phase,fixture,mode,checker_time_us,execution_time_us"
    ));
    for phase in ["baseline-scalar", "aggregate-carrier"] {
        assert!(PRECISION_ARTIFACT.contains(&format!("phase={phase}")), "missing {phase} phase");
    }

    for (fixture, source, expected_output, expected_edges) in [
        ("telemetry_reference_return", RETURN_FIXTURE, "9 8", 807),
        ("telemetry_reference_list", FIXTURE, "resumed resumed", 809),
    ] {
        let checked = witchy::resolve_std_only_checked(source)
            .unwrap_or_else(|error| panic!("resolve {fixture}: {error}"));
        let no_copy_misses = witchy_lower::analysis::module_no_copy_misses(checked.module());
        let repair_entries = witchy_lower::analysis::module_boundary_repairs(checked.module());
        assert_eq!(no_copy_misses.len(), 2, "{fixture} no-copy evidence drifted");
        assert_eq!(repair_entries.len(), 2, "{fixture} repair evidence drifted");

        for (mode, options) in [
            ("optimized", OptSet::default_set()),
            ("forced-copy", OptSet::default_set().without(Opt::InPlace)),
        ] {
            opt::set_for_tests(Some(options));
            let timed = stats::compute_timed(source)
                .unwrap_or_else(|error| panic!("measure {fixture} {mode}: {error}"));
            opt::set_for_tests(None);
            assert_eq!(timed.stats.output, [expected_output], "{fixture} {mode} output drifted");
            assert_eq!(timed.stats.loan_control_flow_edges, expected_edges);
            assert!(timed.checker_time_us > 0, "{fixture} {mode} checker sample missing");
            assert!(timed.execution_time_us > 0, "{fixture} {mode} execution sample missing");
            assert!(timed.stats.heap_bytes >= 0);
            assert!(timed.stats.rc_alloc_calls >= 0);
            assert!(timed.stats.bump_alloc_calls >= 0);
            assert!(timed.stats.loan_opens <= timed.stats.loan_opens + timed.stats.loan_closes);
            let phase = if fixture == "telemetry_reference_return" {
                "baseline-scalar"
            } else {
                "aggregate-carrier"
            };
            assert!(
                PRECISION_ARTIFACT.contains(&format!(
                    "phase={phase},fixture={fixture},mode={mode},checker_time_us=measured"
                )),
                "missing checked timing row for {phase}/{fixture}/{mode}"
            );
            assert!(
                PRECISION_ARTIFACT.contains(&format!(
                    "fixture={fixture},mode={mode},checker_time_us=measured,execution_time_us=measured,peak_memory=heap_bytes,loan_edges={expected_edges},subset_edges=0,no_copy_misses=2,repair_entries=2"
                )),
                "missing checked analysis counters for {fixture}/{mode}"
            );
        }
    }
    opt::set_for_tests(None);
}
