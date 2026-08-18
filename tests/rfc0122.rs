//! RFC-0122 checked-reference evidence.
//!
//! The corpus deliberately records deterministic compiler/runtime counters,
//! never wall-clock timings. It is the acceptance artifact for the reference
//! implementation's optimized and forced-copy paths.

use witchy::interpreter;
use witchy::stats::{self, Stats};
use witchy_syntax::opt::{self, Opt, OptSet};

const REFERENCE_RETURN: &str = include_str!("rfc0122/telemetry_reference_return.witchy");
const EXPECTED: &str = include_str!("rfc0122/telemetry_reference_return.expected");
const REFERENCE_LIST: &str = include_str!("rfc0122/telemetry_reference_list.witchy");
const LIST_EXPECTED: &str = include_str!("rfc0122/telemetry_reference_list.expected");

const LOAN_SCHEMA: &[&str] = &[
    "loan_active_points",
    "loan_active_events",
    "loan_opens",
    "loan_closes",
    "loan_return_transfers",
    "loan_shell_mutations",
    "loan_control_flow_edges",
    "loan_subset_edges",
    "boundary_reown_copies",
    "ownership_token_repairs",
    "direct_storage_var_accesses",
];

const RUNTIME_SCHEMA: &[&str] = &[
    "heap_bytes",
    "reowns",
    "indirect_ownership_calls",
    "boundary_reown_copies",
    "ownership_token_repairs",
    "direct_storage_var_accesses",
    "destination_candidates_forwarded",
    "rc_alloc_calls",
    "bump_alloc_calls",
    "rc_reuse_calls",
    "rc_free_calls",
    "live_cells",
];

fn run(optimizations: OptSet) -> Stats {
    opt::set_for_tests(Some(optimizations));
    let result = stats::compute(REFERENCE_RETURN).expect("compile and run telemetry corpus fixture");
    opt::set_for_tests(None);
    result
}

fn run_list(optimizations: OptSet) -> Stats {
    opt::set_for_tests(Some(optimizations));
    let result = stats::compute(REFERENCE_LIST).expect("compile and run aggregate telemetry corpus fixture");
    opt::set_for_tests(None);
    result
}

fn metric_row(stats: &Stats) -> Vec<i64> {
    vec![
        stats.loan_active_points as i64,
        stats.loan_active_events as i64,
        stats.loan_opens as i64,
        stats.loan_closes as i64,
        stats.loan_return_transfers as i64,
        stats.loan_shell_mutations as i64,
        stats.loan_control_flow_edges as i64,
        stats.loan_subset_edges as i64,
        stats.boundary_reown_copies,
        stats.ownership_token_repairs,
        stats.direct_storage_var_accesses,
    ]
}

fn runtime_row(stats: &Stats) -> Vec<i64> {
    vec![
        stats.heap_bytes,
        stats.reowns,
        stats.indirect_ownership_calls,
        stats.boundary_reown_copies,
        stats.ownership_token_repairs,
        stats.direct_storage_var_accesses,
        stats.destination_candidates_forwarded,
        stats.rc_alloc_calls,
        stats.bump_alloc_calls,
        stats.rc_reuse_calls,
        stats.rc_free_calls,
        stats.live_cells,
    ]
}

#[test]
fn reference_return_telemetry_corpus_pins_schema_and_copy_parity() {
    assert!(EXPECTED.contains(&format!("schema={}", LOAN_SCHEMA.join(","))));
    assert!(EXPECTED.contains("optimized.output=9 8"));
    assert!(EXPECTED.contains("forced_copy.output=9 8"));
    assert!(EXPECTED.contains("interpreter.output=9 8"));

    let optimized = run(OptSet::default_set());
    let forced_copy = run(OptSet::default_set().without(Opt::InPlace));
    let checked = witchy::resolve_std_only_checked(REFERENCE_RETURN)
        .expect("resolve reference-return telemetry fixture");
    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("interpret reference-return telemetry fixture");

    assert_eq!(optimized.output, ["9 8"]);
    assert_eq!(forced_copy.output, optimized.output, "forced-copy lowering must preserve results");
    assert_eq!(interpreted, optimized.output, "interpreter telemetry fixture must preserve output");

    let optimized_row = metric_row(&optimized);
    let forced_copy_row = metric_row(&forced_copy);
    let optimized_runtime = runtime_row(&optimized);
    let forced_copy_runtime = runtime_row(&forced_copy);
    assert!(EXPECTED.contains(&format!("runtime_schema={}", RUNTIME_SCHEMA.join(","))));
    assert!(EXPECTED.contains("optimized.runtime=136,0,0,0,0,0,0,7,7,0,0,7"));
    assert!(EXPECTED.contains("forced_copy.runtime=136,0,0,0,0,0,0,7,7,0,0,7"));
    assert_eq!(optimized_runtime, [136, 0, 0, 0, 0, 0, 0, 7, 7, 0, 0, 7]);
    assert_eq!(forced_copy_runtime, optimized_runtime);
    assert_eq!(
        optimized_row,
        [1, 1, 1, 1, 2, 0, 807, 0, 0, 0, 0],
        "the pinned optimized telemetry artifact changed"
    );
    assert_eq!(
        forced_copy_row,
        optimized_row,
        "forced-copy lowering must retain the same source loan facts"
    );
    assert!(EXPECTED.contains("optimized.metrics=1,1,1,1,2,0,807,0,0,0,0"));
    assert!(EXPECTED.contains("forced_copy.metrics=1,1,1,1,2,0,807,0,0,0,0"));
}

#[test]
fn aggregate_reference_telemetry_corpus_pins_schema_and_copy_parity() {
    assert!(LIST_EXPECTED.contains(&format!("schema={}", LOAN_SCHEMA.join(","))));
    assert!(LIST_EXPECTED.contains("optimized.output=resumed resumed"));
    assert!(LIST_EXPECTED.contains("forced_copy.output=resumed resumed"));
    assert!(LIST_EXPECTED.contains("interpreter.output=resumed resumed"));

    let optimized = run_list(OptSet::default_set());
    let forced_copy = run_list(OptSet::default_set().without(Opt::InPlace));
    let checked = witchy::resolve_std_only_checked(REFERENCE_LIST)
        .expect("resolve aggregate telemetry fixture");
    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("interpret aggregate telemetry fixture");

    assert_eq!(optimized.output, ["resumed resumed"]);
    assert_eq!(forced_copy.output, optimized.output, "forced-copy lowering must preserve results");
    assert_eq!(interpreted, optimized.output, "interpreter telemetry fixture must preserve output");

    let optimized_row = metric_row(&optimized);
    let forced_copy_row = metric_row(&forced_copy);
    let optimized_runtime = runtime_row(&optimized);
    let forced_copy_runtime = runtime_row(&forced_copy);
    assert!(LIST_EXPECTED.contains(&format!("runtime_schema={}", RUNTIME_SCHEMA.join(","))));
    assert!(LIST_EXPECTED.contains("optimized.runtime=157,0,0,0,0,0,0,4,4,0,0,4"));
    assert!(LIST_EXPECTED.contains("forced_copy.runtime=157,0,0,0,0,0,0,4,4,0,0,4"));
    assert_eq!(optimized_runtime, [157, 0, 0, 0, 0, 0, 0, 4, 4, 0, 0, 4]);
    assert_eq!(forced_copy_runtime, optimized_runtime);
    assert_eq!(forced_copy_row, optimized_row, "forced-copy lowering must retain the same source loan facts");
    assert!(
        LIST_EXPECTED.contains(&format!(
            "optimized.metrics={}",
            optimized_row.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
        )),
        "record the optimized metric row in the checked artifact: {optimized_row:?}",
    );
    assert!(
        LIST_EXPECTED.contains(&format!(
            "forced_copy.metrics={}",
            forced_copy_row.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
        )),
        "record the forced-copy metric row in the checked artifact: {forced_copy_row:?}",
    );
}
