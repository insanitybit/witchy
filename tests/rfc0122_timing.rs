//! RFC-0122 measured checker/runtime telemetry.

use witchy::stats;
use witchy_syntax::opt::{self, Opt, OptSet};

const FIXTURE: &str = include_str!("rfc0122/telemetry_reference_list.witchy");

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
