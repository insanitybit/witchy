//! RFC-0030: deterministic optimization counters (`witchy stats`).
//!
//! Compile a program and run it under the active [`crate::opt`] (`WITCHY_OPT`)
//! setting, returning exact operation *counts* plus an opt-in timing view so an
//! optimization's effect is a unit-testable fact rather than a flaky benchmark.
//! A forced-copy run (`WITCHY_OPT=-inplace`) re-owns at every accumulation site
//! and allocates O(n^2) heap; an in-place run re-owns far fewer times and
//! allocates O(n). The differential sweep proves an optimization changes nothing
//! *observable*; these counters prove it actually *fired*.
#![cfg(feature = "native")]

use witchy_runtime::runtime::{Capabilities, Runtime};
use witchy_lower::codegen;
use std::time::Instant;
#[cfg(test)]
use witchy_types::typeck;

/// Wasm linear-memory pages for a stats run (mirrors the CLI's run budget,
/// including a `@large_heap` program's 32 GiB).
fn stats_memory_pages(artifact: &witchy_wir::AdmittedCanonicalArtifact) -> usize {
    match artifact.heap_mode() {
        witchy_wir::HeapMode::Standard => 16384,
        mode @ witchy_wir::HeapMode::Large => mode.max_pages() as usize,
    }
}


/// Deterministic counters from one program run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stats {
    /// The program's printed output — so a stats run also pins behavior.
    pub output: Vec<String>,
    /// Final `$heap` frontier in bytes (the peak, with no reclaim).
    pub heap_bytes: i64,
    /// Accumulation sites that entered with a zero ownership token — each copies.
    /// Small on a clean in-place run; ~one-per-iteration under `WITCHY_OPT=-inplace`.
    pub reowns: i64,
    /// RFC-0110 calls whose ownership envelope crossed a real indirect table
    /// boundary (excluding direct and devirtualized closure calls).
    pub indirect_ownership_calls: i64,
    /// Normal-mode uniqueness repairs. An accepted opt-mode call records zero;
    /// its paired normal-mode alias case records exactly one.
    pub boundary_reown_copies: i64,
    /// Ownership-state inputs reconstructed at a boundary instead of forwarded.
    pub ownership_token_repairs: i64,
    /// Exclusive `var` calls which operated directly on caller storage.
    pub direct_storage_var_accesses: i64,
    /// RFC-0111 unique-result destinations forwarded into their producer.
    pub destination_candidates_forwarded: i64,
    /// Descriptor-owned packed allocation counters. Box/reshape metrics are
    /// added only with real adapter increment sites and nonzero fixtures.
    pub packed_alloc_calls: i64,
    pub packed_alloc_bytes: i64,
    /// Bytes moved by `region:` copy-outs.
    pub region_copy_bytes: i64,
    /// (RFC-0016) Bytes `$rc_alloc` reused from the RC-floor free-list rather than
    /// freshly bumping — 0 unless the free-at-overwrite rule (gated `rc-floor`)
    /// reclaimed a dead buffer that a later allocation then recycled.
    pub rc_reused_bytes: i64,
    /// (RFC-0035) Live rc_alloc objects at program end (`$rc_alloc` +1, `$rc_free` -1).
    /// A leak metric: bounded for a fully-reclaiming rc-floor program, growing with the
    /// input for an unbounded leak. 0 unless a `$rc_free` fired.
    pub live_cells: i64,
    /// RFC-0089 allocator/reclaimer operation counts. For an FIP kernel these
    /// stay fixed as only the kernel's recursive depth increases.
    pub rc_alloc_calls: i64,
    /// Logical payload bytes requested through `$rc_alloc`; this excludes the
    /// allocator header and remains nonzero when a request is satisfied from
    /// the free list.
    pub rc_alloc_requested_bytes: i64,
    pub bump_alloc_calls: i64,
    pub rc_reuse_calls: i64,
    pub rc_free_calls: i64,
    pub region_rewind_calls: i64,
    /// RFC-0146 indexed-sequence kernel operations. Header loads happen once
    /// per entered stable plan; the remaining counts are dynamic accesses or
    /// transformations performed by the loop-local plan.
    pub list_header_loads: i64,
    pub checked_indexed_loads: i64,
    pub checked_indexed_stores: i64,
    pub cursorized_indexed_accesses: i64,
    pub sequence_bounds_checks_coalesced: i64,
    pub sequence_forwarded_loads: i64,
    pub sequence_small_loops_unrolled: i64,
    /// Executed state transitions in the ordinary `task.run` scheduler.
    pub scheduler_slot_transitions: i64,
    /// Wasm-GC allocations of compiler-owned `task.Step` frame entries.
    ///
    /// This counts only the exact `Pull*Frame*` structs created at their
    /// compiler-owned scheduler boundary. It deliberately excludes generic
    /// RC traffic, user values, closures, frame-lane lists, and in-place slot
    /// retags. Wasm-GC does not provide a stable object-byte size, so this is
    /// a call count rather than a fabricated byte estimate. It also cannot
    /// prove allocations for a future typed direct-ring emitter, which has no
    /// current compiler-owned allocation boundary.
    pub scheduler_frame_allocation_calls: i64,
    /// Subset of [`Self::scheduler_frame_allocation_calls`] whose entry is an
    /// eager `Pull*ReadyFrame*` probe. The entry can still park; ready versus
    /// blocking execution is established by the wait counters separately.
    pub scheduler_ready_frame_allocation_calls: i64,
    /// Executed transitions into one of the ordinary scheduler's wait states.
    pub scheduler_wait_installs: i64,
    /// A parked scheduler operation resumed after a concrete ready or close
    /// outcome was claimed. Re-parking a reusable frame is not a claim.
    pub scheduler_wake_claims: i64,
    /// One-time installations of compiler-owned reusable select wait frames.
    pub scheduler_reusable_wait_installs: i64,
    /// Live compiler-owned frame wait registrations at program completion.
    /// This is zero for a completed bounded run; a `task.run` slot is the
    /// registration identity.
    pub scheduler_frame_live_wait_registrations: i64,
    /// High-water compiler-owned frame wait registrations. Re-arming a parked
    /// slot does not increase the value.
    pub scheduler_frame_peak_live_wait_registrations: i64,
    /// Compiler-frame wait-to-wait slot retags that preserve a registration
    /// identity.
    pub scheduler_frame_wait_registration_rearms: i64,
    /// Nominal `Selected*` values constructed at a select escape boundary.
    /// Non-escaping direct result lanes must leave this at zero.
    pub selected_result_materializations: i64,
    /// RFC-0088 semantic dictionary searches performed by fused extraction.
    pub extract_searches: i64,
    /// Key comparisons made within those searches.
    pub extract_key_comparisons: i64,
    /// Structural bytes copied by update-and-extract helpers.
    pub extract_copied_bytes: i64,
    /// RC-backed leaves retained by extraction's shared-storage path.
    pub extract_retains: i64,
    /// RC-backed leaves released by extraction's structural repair.
    pub extract_drops: i64,
    /// RFC-0143 Swiss probe attribution counters (active during extraction runs).
    pub dict_hashes: i64,
    pub dict_probe_groups: i64,
    pub dict_h2_candidates: i64,
    pub dict_rebuilds: i64,
    pub dict_grows: i64,
    /// RFC-0143 bytes moved by ordered dictionary projections/removal repair.
    pub dict_order_bytes_moved: i64,
    pub dict_index_bytes: i64,
    /// Checked program points carrying one or more active loan facts.
    pub loan_active_points: usize,
    /// Total active loan facts across those program points.
    pub loan_active_events: usize,
    /// Borrow relations opened and closed by the checker.
    pub loan_opens: usize,
    pub loan_closes: usize,
    /// Checked return transfers and fixed-shell mutation facts.
    pub loan_return_transfers: usize,
    pub loan_shell_mutations: usize,
    /// Control-flow and origin-subset relations published by the checker.
    pub loan_control_flow_edges: usize,
    pub loan_subset_edges: usize,
}

/// Deterministic counters plus one measured compiler/runtime sample.
///
/// `Stats` remains the stable counter-only API. Timings are deliberately kept
/// in this separate result so callers that compare optimization facts do not
/// accidentally make wall-clock measurements part of deterministic tests.
#[derive(Debug, Clone)]
pub struct TimedStats {
    pub stats: Stats,
    /// Wasmtime GC backing-heap capacity after execution. Under the current
    /// deferred-reference-counting collector this is the VM-lifetime capacity
    /// high-water, not live-object bytes or process RSS.
    pub gc_heap_capacity_bytes: usize,
    /// Microseconds spent resolving, checking, and publishing loan facts.
    pub checker_time_us: u128,
    /// Microseconds spent executing the compiled Wasm sample.
    pub execution_time_us: u128,
    /// Completed batch executions per second for this single-run sample.
    ///
    /// This is a throughput-shaped companion to `execution_time_us`, not a
    /// benchmark claim: one successful `vm.run()` is the measured unit and
    /// callers should compare repeated samples when making performance claims.
    pub execution_throughput_batches_per_s: f64,
}

/// Compile `src` (resolved against the bundled std) and run it under the active
/// `WITCHY_OPT` setting, returning its deterministic counters. The stats
/// harness grants `Console` and the deterministic local `Clock` capability, so
/// benchmark kernels may measure an in-program interval without requiring a
/// different runner. All other capabilities remain denied.
pub fn compute(src: &str) -> Result<Stats, String> {
    compute_timed(src).map(|timed| timed.stats)
}

/// A compiled program plus the checker facts gathered while producing it.
///
/// Compilation is deterministic in the source, so a caller that samples the
/// same program repeatedly (a timing loop) should build this ONCE and hand it
/// to `run_prepared` per sample. Repeating the resolve/check/lower work per
/// sample measures nothing extra -- `execution_time_us` is timed around
/// `vm.run()` alone -- and it dominated the cost of the RFC-0129 acceptance
/// tests, which recompiled two whole programs on every repetition.
pub struct PreparedProgram {
    artifact: witchy_wir::AdmittedCanonicalArtifact,
    checker_time_us: u128,
    loan_telemetry: witchy_types::loans::LoanFactTelemetry,
}

/// Resolve `src` against the bundled std, publish its loan facts, and lower it
/// to Wasm. See `PreparedProgram` for why this is separable from running.
pub fn prepare(src: &str) -> Result<PreparedProgram, String> {
    let checker_started = Instant::now();
    let checked = crate::resolve_std_only_checked(src).map_err(|error| error.to_string())?;
    let loan_telemetry = witchy_types::loans::facts(checked.module())
        .map_err(|error| error.message)?
        .telemetry();
    let checker_time_us = checker_started.elapsed().as_micros();
    // Canonical lowering does not use the legacy sequence-counter switch.
    // Retain its sealed entry and owner bridges, not just its byte projection.
    let artifact = match codegen::compile_checked_module_artifact(&checked) {
        codegen::LoweringOutcome::Lowered(artifact) => artifact,
        codegen::LoweringOutcome::Unsupported(reason) => return Err(reason.to_string()),
        codegen::LoweringOutcome::Rejected(error) => return Err(error.message),
    };
    Ok(PreparedProgram { artifact, checker_time_us, loan_telemetry })
}

/// Run an already-compiled program and collect its deterministic counters.
///
/// Each call spawns a FRESH VM, so the allocation counters describe this run
/// alone -- that isolation is what the RFC-0129 gates assert on, and it is
/// preserved exactly when a caller reuses one `PreparedProgram`.
pub fn run_prepared(prepared: &PreparedProgram) -> Result<TimedStats, String> {
    let caps = Capabilities {
        print: true,
        print_int: true,
        quiet: true,
        clock: true,
        ..Default::default()
    };
    let mut rt = Runtime::batch().map_err(|e| e.to_string())?;
    let mut vm = rt
        .spawn_admitted(&prepared.artifact, caps, stats_memory_pages(&prepared.artifact))
        .map_err(|e| e.to_string())?;
    let execution_started = Instant::now();
    vm.run().map_err(|e| e.root_cause().to_string())?;
    let execution_time_us = execution_started.elapsed().as_micros();
    let execution_throughput_batches_per_s =
        1_000_000.0 / execution_time_us.max(1) as f64;
    let gc_heap_capacity_bytes = vm.gc_heap_capacity_bytes();
    let stats = Stats {
        output: vm.output(),
        heap_bytes: vm.heap_bytes().unwrap_or(0),
        reowns: vm.reowns().unwrap_or(0),
        indirect_ownership_calls: vm.indirect_ownership_calls().unwrap_or(0),
        boundary_reown_copies: vm.boundary_reown_copies().unwrap_or(0),
        ownership_token_repairs: vm.ownership_token_repairs().unwrap_or(0),
        direct_storage_var_accesses: vm.direct_storage_var_accesses().unwrap_or(0),
        destination_candidates_forwarded: vm.destination_candidates_forwarded().unwrap_or(0),
        packed_alloc_calls: vm.packed_alloc_calls().unwrap_or(0),
        packed_alloc_bytes: vm.packed_alloc_bytes().unwrap_or(0),
        region_copy_bytes: vm.region_copy_bytes().unwrap_or(0),
        rc_reused_bytes: vm.rc_reused_bytes().unwrap_or(0),
        live_cells: vm.live_cells().unwrap_or(0),
        rc_alloc_calls: vm.rc_alloc_calls().unwrap_or(0),
        rc_alloc_requested_bytes: vm.rc_alloc_requested_bytes().unwrap_or(0),
        bump_alloc_calls: vm.bump_alloc_calls().unwrap_or(0),
        rc_reuse_calls: vm.rc_reuse_calls().unwrap_or(0),
        rc_free_calls: vm.rc_free_calls().unwrap_or(0),
        region_rewind_calls: vm.region_rewind_calls().unwrap_or(0),
        list_header_loads: vm.list_header_loads().unwrap_or(0),
        checked_indexed_loads: vm.checked_indexed_loads().unwrap_or(0),
        checked_indexed_stores: vm.checked_indexed_stores().unwrap_or(0),
        cursorized_indexed_accesses: vm.cursorized_indexed_accesses().unwrap_or(0),
        sequence_bounds_checks_coalesced: vm.sequence_bounds_checks_coalesced().unwrap_or(0),
        sequence_forwarded_loads: vm.sequence_forwarded_loads().unwrap_or(0),
        sequence_small_loops_unrolled: vm.sequence_small_loops_unrolled().unwrap_or(0),
        scheduler_slot_transitions: vm.scheduler_slot_transitions().unwrap_or(0),
        scheduler_frame_allocation_calls: vm.scheduler_frame_allocation_calls().unwrap_or(0),
        scheduler_ready_frame_allocation_calls: vm
            .scheduler_ready_frame_allocation_calls()
            .unwrap_or(0),
        scheduler_wait_installs: vm.scheduler_wait_installs().unwrap_or(0),
        scheduler_wake_claims: vm.scheduler_wake_claims().unwrap_or(0),
        scheduler_reusable_wait_installs: vm.scheduler_reusable_wait_installs().unwrap_or(0),
        scheduler_frame_live_wait_registrations: vm
            .scheduler_frame_live_wait_registrations()
            .unwrap_or(0),
        scheduler_frame_peak_live_wait_registrations: vm
            .scheduler_frame_peak_live_wait_registrations()
            .unwrap_or(0),
        scheduler_frame_wait_registration_rearms: vm
            .scheduler_frame_wait_registration_rearms()
            .unwrap_or(0),
        selected_result_materializations: vm.selected_result_materializations().unwrap_or(0),
        extract_searches: vm.extract_searches().unwrap_or(0),
        extract_key_comparisons: vm.extract_key_comparisons().unwrap_or(0),
        extract_copied_bytes: vm.extract_copied_bytes().unwrap_or(0),
        extract_retains: vm.extract_retains().unwrap_or(0),
        extract_drops: vm.extract_drops().unwrap_or(0),
        dict_hashes: vm.dict_hashes().unwrap_or(0),
        dict_probe_groups: vm.dict_probe_groups().unwrap_or(0),
        dict_h2_candidates: vm.dict_h2_candidates().unwrap_or(0),
        dict_rebuilds: vm.dict_rebuilds().unwrap_or(0),
        dict_grows: vm.dict_grows().unwrap_or(0),
        dict_order_bytes_moved: vm.dict_order_bytes_moved().unwrap_or(0),
        dict_index_bytes: vm.dict_index_bytes().unwrap_or(0),
        loan_active_points: prepared.loan_telemetry.active_points,
        loan_active_events: prepared.loan_telemetry.active_events,
        loan_opens: prepared.loan_telemetry.opens,
        loan_closes: prepared.loan_telemetry.closes,
        loan_return_transfers: prepared.loan_telemetry.return_transfers,
        loan_shell_mutations: prepared.loan_telemetry.shell_mutations,
        loan_control_flow_edges: prepared.loan_telemetry.control_flow_edges,
        loan_subset_edges: prepared.loan_telemetry.subset_edges,
    };
    Ok(TimedStats {
        stats,
        gc_heap_capacity_bytes,
        checker_time_us: prepared.checker_time_us,
        execution_time_us,
        execution_throughput_batches_per_s,
    })
}

/// Compile and run a stats corpus while retaining measured checker and
/// execution durations for performance reports. The returned counters remain
/// deterministic; callers must treat the two duration fields as measurements.
pub fn compute_timed(src: &str) -> Result<TimedStats, String> {
    run_prepared(&prepare(src)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use witchy_syntax::opt::{self, Opt, OptSet};

    #[test]
    fn internal_admitted_launch_preserves_stats_owner_bridges() {
        let source = r#"
fn make(own text: String, console: Console) -> fn() -> ():
    let captured = fn(): console.print(text)
    console.print(text)
    captured

fn main(console: Console):
    let captured = make("held", console)
    captured()
    captured()
"#;
        let checked = crate::resolve_std_only_checked(source).expect("checked owner capture");
        let expected = vec!["held".to_owned(); 3];
        let prepared = prepare(source).expect("prepare admitted owner capture");
        assert!(!prepared.artifact.owner_bridges().is_empty());
        for _ in 0..2 {
            assert_eq!(run_prepared(&prepared).expect("fresh admitted stats instance").stats.output, expected);
        }
    }

    // `heap_bytes` is the allocator's high-water above its heap base, so a
    // loop that reclaims its scratch keeps it flat as iterations grow.
    fn assert_bounded_workload_growth(small: &Stats, large: &Stats, tolerance: i64, label: &str) {
        let (small_workload, large_workload) = (small.heap_bytes, large.heap_bytes);
        assert!(small_workload > 0, "{label}: the workload allocates: {small_workload}");
        assert!(
            large_workload <= small_workload + tolerance,
            "{label}: workload heap grew beyond tolerance: small={small_workload}, large={large_workload}, tolerance={tolerance}"
        );
    }

    #[test]
    fn stats_grants_clock_for_in_program_benchmark_kernels() {
        let stats = compute(
            "fn main(console: Console, clock: Clock):\n    let started = clock.now_monotonic()\n    let finished = clock.now_monotonic()\n    console.print(\"${finished >= started}\")\n",
        )
        .expect("stats must run a clock-based benchmark kernel");
        assert_eq!(stats.output, ["true"]);
    }

    #[test]
    fn deterministic_counters_repeat_exactly() {
        const SOURCE: &str = "fn main(console: Console):\n    var xs = []\n    var i = 0\n    while i < 64:\n        xs.push(i)\n        i = i + 1\n    console.print(\"${xs.length()}\")\n";
        opt::set_for_tests(Some(OptSet::default_set()));
        let runs = repeat_runs(SOURCE, 3).expect("three deterministic counter runs");
        let (first, second, third) = (&runs[0], &runs[1], &runs[2]);
        opt::set_for_tests(None);
        assert_eq!(first.output, ["64"]);
        assert_eq!(first, second, "counter run one and two must agree exactly");
        assert_eq!(second, third, "counter run two and three must agree exactly");
    }

    // An accumulation loop: in-place push keeps its heap O(n); a copy per
    // push would make it O(n^2).
    fn accumulation(elements: usize) -> String {
        format!("fn main(console: Console):\n    var xs = []\n    var i = 0\n    while i < {elements}:\n        list.push(xs, i)\n        i = i + 1\n    console.print(\"${{list.length(xs)}}\")\n")
    }

    // A soak loop: per-iteration scratch that never escapes is reclaimed each
    // iteration, so the heap stays flat no matter how many iterations run.
    fn soak(iterations: usize) -> String {
        format!("fn main(console: Console):\n    var sum = 0\n    var i = 0\n    while i < {iterations}:\n        let tmp = [i, i + 1, i + 2, i + 3]\n        sum = sum + list.length(tmp)\n        i = i + 1\n    console.print(\"${{sum}}\")\n")
    }

    fn fip_kernel(steps: usize) -> String {
        format!(
            "mode opt\n\n\
             type State:\n    count: Int\n    limit: Int\n\n\
             fn run(own state: unique State, n: Int) -> unique State:\n\
             \x20   if n == 0:\n        return state\n\
             \x20   state.count = state.count + 1\n\
             \x20   run(state, n - 1)\n\n\
             fn main(console: Console):\n\
             \x20   let done = run(State(0, {steps}), {steps})\n\
             \x20   console.print(\"${{done.count}}\")\n"
        )
    }

    fn record_projection_kernel(iterations: usize) -> String {
        format!(
            "mode opt\n\n\
             type Summary = .{{id: Int, label: String}}\n\
             type Detailed = .{{..Summary, revision: Int}}\n\n\
             fn main(console: Console):\n\
             \x20   let row: Detailed = .{{id: 7, label: \"ready\", revision: 3}}\n\
             \x20   var i = 0\n\
             \x20   var total = 0\n\
             \x20   while i < {iterations}:\n\
             \x20       let summary: Summary = row\n\
             \x20       total = total + summary.id\n\
             \x20       i = i + 1\n\
             \x20   console.print(\"${{total}} ${{row.revision}}\")\n"
        )
    }

    /// In-place push keeps an accumulation's heap linear: ten times the
    /// elements stays well under the hundredfold growth a copy per push makes.
    #[test]
    fn accumulation_heap_grows_linearly() {
        let small = compute(&accumulation(400)).expect("compute 400 pushes");
        let large = compute(&accumulation(4000)).expect("compute 4000 pushes");
        assert_eq!(small.output, vec!["400".to_string()]);
        assert_eq!(large.output, vec!["4000".to_string()]);
        assert!(small.heap_bytes > 0, "the accumulation allocates: {}", small.heap_bytes);
        assert!(
            large.heap_bytes < small.heap_bytes * 20,
            "accumulation heap must grow linearly: 400={} 4000={}",
            small.heap_bytes,
            large.heap_bytes
        );
    }

    /// A list of strings appends in place too: copying the whole list per
    /// push made 4000 appends cost a hundredfold the heap of 400.
    fn string_accumulation(elements: usize) -> String {
        format!("fn main(console: Console):\n    var xs = []\n    var i = 0\n    while i < {elements}:\n        xs.push(\"s${{i % 10}}\")\n        i = i + 1\n    console.print(\"${{xs.length()}}\")\n")
    }

    #[test]
    fn string_accumulation_heap_grows_linearly() {
        let small = compute(&string_accumulation(400)).expect("compute 400 string pushes");
        let large = compute(&string_accumulation(4000)).expect("compute 4000 string pushes");
        assert_eq!(large.output, vec!["4000".to_string()]);
        assert!(
            large.heap_bytes < small.heap_bytes * 20,
            "string accumulation heap must grow linearly: 400={} 4000={}",
            small.heap_bytes,
            large.heap_bytes
        );
    }

    fn field_string_accumulation(elements: usize) -> String {
        format!("type Bag:\n    items: List(String)\n\nfn main(console: Console):\n    var bag = Bag([])\n    var i = 0\n    while i < {elements}:\n        bag.items.push(\"s${{i % 10}}\")\n        i = i + 1\n    console.print(\"${{bag.items.length()}}\")\n")
    }

    #[test]
    fn field_string_accumulation_heap_grows_linearly() {
        let small = compute(&field_string_accumulation(400)).expect("compute 400 field pushes");
        let large = compute(&field_string_accumulation(4000)).expect("compute 4000 field pushes");
        assert_eq!(large.output, vec!["4000".to_string()]);
        assert!(
            large.heap_bytes < small.heap_bytes * 20,
            "field string accumulation heap must grow linearly: 400={} 4000={}",
            small.heap_bytes,
            large.heap_bytes
        );
    }

    fn accepted_opt_stats(source: &str, label: &str) -> Stats {
        let linked = crate::resolve_std_only(source).unwrap_or_else(|error| {
            panic!("resolve {label}: {error}")
        });
        crate::enforce_performance_modes(&linked, "main").unwrap_or_else(|error| {
            panic!("{label} must satisfy mode opt: {error}")
        });
        opt::set_for_tests(Some(OptSet::default_set()));
        let stats = compute(source).unwrap_or_else(|error| panic!("run {label}: {error}"));
        opt::set_for_tests(None);
        stats
    }

    #[test]
    fn existential_var_dispatch_rejects_an_aliased_capacity() {
        const SOURCE: &str = "mode opt\n\ntrait Appender:\n    fn append(let self, var xs: unique List(Int)) -> ()\n\ntype Step:\n    amount: Int\n\nimpl Appender for Step:\n    fn append(let self: Step, var xs: unique List(Int)) -> ():\n        xs.push(self.amount)\n\nfn main():\n    let append: dyn Appender = Step(9)\n    var xs = [1]\n    let alias = xs\n    append.append(xs)\n    let _ = alias\n";

        let linked = crate::resolve_std_only(SOURCE).expect("resolve aliased existential call");
        let error = crate::enforce_performance_modes(&linked, "main")
            .expect_err("an existential var call may not receive an aliased capacity");
        assert!(error.contains("existential dispatch"), "{error}");
        assert!(error.contains("bound to a new name"), "{error}");
    }

    /// RFC-0030 soak / never-OOM guard: a long loop whose per-iteration
    /// scratch is escape-free runs in bounded heap, reclaimed each iteration.
    #[test]
    fn soak_heap_stays_bounded() {
        let small = compute(&soak(500)).expect("compute 500 iterations");
        let large = compute(&soak(5000)).expect("compute 5000 iterations");
        assert_eq!(small.output, vec!["2000".to_string()]);
        assert_eq!(large.output, vec!["20000".to_string()]);
        assert_bounded_workload_growth(&small, &large, 64, "the soak loop's heap must stay bounded");
    }

    /// RFC-0030 differential de-opt sweep (DoD half (a)): a program's OUTPUT must
    /// be identical under every `WITCHY_OPT` setting — `none`, `all`, the default,
    /// and the default minus each optimization — with asserted output.
    /// Toggling an optimization changes *how* a program runs, never *what*
    /// it computes; walking `Opt::ALL` covers new optimizations automatically.
    #[test]
    fn differential_sweep_output_is_invariant_on_both_backends() {
        // A corpus that exercises every shipped optimization and surface feature:
        // accumulation (inplace), string/dict building, escape-free scratch
        // (region), constant folding (fold), `for var` write-back, and
        // `nodes.push(x)` mutating-method statements.
        let corpus = [
            // accumulation (inplace) + string build + dict update + fold, with a
            // `nodes.push` statement and escape-free loop scratch (region).
            "fn main(console: Console):\n    var xs = []\n    var s = \"\"\n    var d = dict.new()\n    var i = 0\n    while i < 300:\n        let scratch = [i, i + 1]\n        xs.push(i + list.length(scratch) - 2)\n        s = s + \"${i % 10}\"\n        let _ = dict.update(d, i % 7, 0, fn(n: Int): n + 1)\n        i = i + 1\n    let folded = 2 * 3 + 4\n    console.print(\"${list.length(xs)}\")\n    console.print(\"${s.length()}\")\n    console.print(\"${dict.get_or(d, 3, 0)}\")\n    console.print(\"${folded}\")\n",
            // `for var` write-back over record elements.
            "import list\ntype P:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    var ps = [P(1, 2), P(3, 4)]\n    for var p in ps:\n        p.x = p.x + 100\n    console.print(\"${list.at(ps, 0).x} ${list.at(ps, 1).x}\")\n",
            // confined slice VIEW: a read-only window over an unmutated param,
            // read only via at/length — toggling `views` must not change output.
            "import list\n\nfn win(xs: List(Int), lo: Int, hi: Int) -> Int:\n    let w = list.slice(xs, lo, hi)\n    var t = 0\n    var j = 0\n    while j < list.length(w):\n        t = t + list.at(w, j)\n        j = j + 1\n    t\n\nfn main(console: Console):\n    let xs = [10, 20, 30, 40, 50, 60]\n    console.print(\"${win(xs, 1, 4)}\")\n    console.print(\"${win(xs, 4, 100)}\")\n    console.print(\"${win(xs, 2, 2)}\")\n",
            // PACKED confined record-list: a list literal of fixed-scalar records
            // read only via at(_).field / length — toggling `unbox` (on under
            // `all`) must not change output.
            "import list\n\ntype P:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let ps = [P(1, 2), P(3, 4), P(5, 6)]\n    var t = 0\n    var i = 0\n    while i < list.length(ps):\n        t = t + list.at(ps, i).x * 10 + list.at(ps, i).y\n        i = i + 1\n    console.print(\"${t}\")\n    console.print(\"${list.length(ps)}\")\n",
            // RC-floor REUSE: a confined var reassigned to same-length list literals,
            // read only via at/length — toggling `rc-elide` (in-place overwrite vs
            // fresh alloc) must not change output.
            "import list\n\nfn main(console: Console):\n    var v = [0, 0, 0]\n    var i = 0\n    while i < 5:\n        v = [i, i * 2, i * 3]\n        i = i + 1\n    console.print(\"${list.at(v, 0) + list.at(v, 1) + list.at(v, 2)}\")\n",
            // RC-floor REUSE (record): a confined var reassigned to the same ctor,
            // read only via fields — `rc-elide` overwrites the field slots in place.
            "type Point:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    var p = Point(0, 0)\n    var i = 0\n    while i < 5:\n        p = Point(i, i * 2)\n        i = i + 1\n    console.print(\"${p.x + p.y}\")\n",
            // (RFC-0033 R1) escaping record updated via `.field =` sugar in a loop:
            // a heap record (returned, so not SROA-confined) updates in place when
            // uniquely owned — output must be invariant under toggling `inplace`.
            "type Counter:\n    count: Int\n    pad: Int\n\nfn build(n: Int) -> Counter:\n    var c = Counter(0, 0)\n    var i = 0\n    while i < n:\n        c.count = c.count + 1\n        i = i + 1\n    c\n\nfn main(console: Console):\n    console.print(\"${build(50).count}\")\n",
            // (RFC-0033) ALIASED record then `.field =`: value semantics — the alias
            // must see the OLD value, so the update re-owns rather than mutating a
            // shared record in place. The expected output is "1 99"; an unsound
            // in-place would print "99 99" and fail this differential.
            "type R:\n    a: Int\n    b: Int\n\nfn main(console: Console):\n    var r = R(1, 2)\n    let alias = r\n    r.a = 99\n    console.print(\"${alias.a} ${r.a}\")\n",
            // (RFC-0033 R3) own-ABI threading through a user record + a PLAIN call
            // to an own-ABI fn (`id(Counter(7,0))`) — both must be invariant under
            // toggling `inplace` and match the asserted output.
            "type Counter:\n    count: Int\n    pad: Int\n\nfn bump(own c: Counter) -> Counter:\n    c.count = c.count + 1\n    c\n\nfn id(own c: Counter) -> Counter:\n    c\n\nfn build(n: Int) -> Counter:\n    var c = Counter(0, 0)\n    var i = 0\n    while i < n:\n        c = bump(c)\n        i = i + 1\n    c\n\nfn main(console: Console):\n    console.print(\"${build(30).count}\")\n    console.print(\"${id(Counter(7, 0)).count}\")\n",
            // CACHE EVICTION: insert then remove distinct dict keys (the per-object
            // RC floor's target garbage). Output must stay invariant under every
            // `WITCHY_OPT` setting and match the asserted output — the behavior guard for
            // the residual the floor will eventually bound (see
            // `cache_eviction_leaks_without_rc_floor`).
            "import dict\n\nfn main(console: Console):\n    var d = dict.new()\n    var i = 0\n    while i < 40:\n        let _ = dict.insert(d, i, i * 2)\n        let _ = dict.remove(d, i)\n        i = i + 1\n    console.print(\"${dict.length(d)}\")\n    let _ = dict.insert(d, 7, 70)\n    console.print(\"${dict.get_or(d, 7, 0)}\")\n",
            // (RFC-0033 R2) FIELD-PATH list push: `s.items = list.push(s.items, x)`
            // grows the field's list buffer in place (build), AND a FIELD-ALIASED case
            // (`let snap = s.items`) that must NOT mutate the snapshot — value
            // semantics pin "102". The field-push-safe gate disables R2 once the field
            // is read for the snapshot; an unsound in-place would print "202" (caught
            // here, e.g. under `-sroa`, which is how the naive R2 was rejected).
            "type Stack:\n    items: List(Int)\n    size: Int\n\nfn build(n: Int) -> Stack:\n    var s = Stack([], 0)\n    var i = 0\n    while i < n:\n        list.push(s.items, i)\n        i = i + 1\n    s\n\nfn aliased() -> Int:\n    var s = Stack([], 0)\n    list.push(s.items, 1)\n    let snap = s.items\n    list.push(s.items, 2)\n    list.length(snap) * 100 + list.length(s.items)\n\nfn main(console: Console):\n    console.print(\"${list.length(build(50).items)}\")\n    console.print(\"${aliased()}\")\n",
            // (RFC-0033 R2) WHOLE-RECORD alias of a field-push record: `let x = s`
            // then another `s.items = list.push(s.items, …)`. The field-push-safe gate
            // PASSES (s.items is read only as the push receiver), so the second guard —
            // `eff = field_cap * (record owned)` — must force a field copy because the
            // record is no longer uniquely owned, leaving the alias's `x.items` length
            // at 1. An unsound in-place would grow x's shared buffer to 2.
            "type Stack:\n    items: List(Int)\n    size: Int\n\nfn whole() -> Stack:\n    var s = Stack([], 0)\n    list.push(s.items, 1)\n    let x = s\n    list.push(s.items, 2)\n    s.size = list.length(x.items)\n    s\n\nfn main(console: Console):\n    let r = whole()\n    console.print(\"${r.size}\")\n    console.print(\"${list.length(r.items)}\")\n",
            // (RFC-0034 L3) Closure devirtualization: `g` is a single-bound CAPTURING
            // closure (captures `k`) called in a loop — devirtualized to a direct
            // `call $__lamw`, the env (so the capture) must still flow, so toggling
            // `direct-call` must not change output. `f` is REASSIGNED mid-loop, so it
            // must stay an indirect call (an unsound devirt would pin the first lambda
            // and diverge once `f` is rebound). The asserted output checks both.
            "fn main(console: Console):\n    let k = 10\n    let g = fn(x: Int): x + k\n    var f = fn(x: Int): x * 2\n    var i = 0\n    var acc = 0\n    while i < 8:\n        acc = acc + g(i) + f(i)\n        if i == 4:\n            f = fn(x: Int): x * 3\n        i = i + 1\n    console.print(\"${acc}\")\n",
            // (RFC-0034 L2) Bounds-check elision: `for i in 0..list.length(xs)` indexing
            // an unmutated `xs` lowers `xs[i]` to an UNCHECKED load (provably in range);
            // toggling `bounds-elide` swaps checked/unchecked codegen and must not change
            // output. Two elidable loops over distinct lists have a fixed
            // expected sum, so an unsound out-of-range load would diverge here.
            "fn main(console: Console):\n    let xs = [3, 1, 4, 1, 5, 9, 2, 6]\n    let ys = [10, 20]\n    var t = 0\n    for i in 0..list.length(xs):\n        t = t + xs[i] * i\n    for j in 0..list.length(ys):\n        t = t + ys[j]\n    console.print(\"${t}\")\n",
        ];
        let expected: &[&[&str]] = &[
            &["300", "300", "43", "10"],
            &["101 103"],
            &["90", "110", "0"],
            &["102", "3"],
            &["24"],
            &["12"],
            &["50"],
            &["1 99"],
            &["30", "7"],
            &["0", "70"],
            &["50", "102"],
            &["1", "2"],
            &["182"],
            &["161"],
        ];
        assert_eq!(corpus.len(), expected.len());
        std::thread::scope(|s| {
            let handles: Vec<_> = corpus.iter().zip(expected).map(|(src, expected)| {
                s.spawn(move || {
                    opt::set_for_tests(Some(OptSet::all()));
                    let base = compute(src).expect("compute all").output;
                    opt::set_for_tests(None);
                    assert_eq!(base, *expected, "admitted Wasm output differs from expected behavior for:\n{src}");

                    let mut settings: Vec<(String, OptSet)> = vec![
                        ("none".into(), OptSet::none()),
                        ("default".into(), OptSet::default_set()),
                    ];
                    for o in Opt::ALL {
                        settings.push((format!("-{}", o.name()), OptSet::default_set().without(o)));
                    }
                    for (label, set) in settings {
                        opt::set_for_tests(Some(set));
                        let out = compute(src).expect("compute").output;
                        opt::set_for_tests(None);
                        assert_eq!(out, base, "WITCHY_OPT={label} changed observable output for:\n{src}");
                    }
                })
            }).collect();
            for h in handles {
                h.join().unwrap();
            }
        });
    }

    /// Compile `src` ONCE, then run it `runs` times and return each run's counters.
    ///
    /// The repeat fixtures below assert that the counter *instrumentation* is
    /// deterministic: the same program must report the same counters on every
    /// run. That is a claim about the runtime, so recompiling per run measured
    /// nothing extra -- and it never covered codegen determinism either, because
    /// `Stats` compares counters rather than module bytes, so two differing
    /// modules with equal counters would have passed just the same.
    /// `codegen_is_byte_deterministic` pins that property directly, once,
    /// instead of charging every fixture a second full compile for it.
    fn repeat_runs(src: &str, runs: usize) -> Result<Vec<Stats>, String> {
        let prepared = prepare(src)?;
        (0..runs)
            .map(|_| run_prepared(&prepared).map(|timed| timed.stats))
            .collect()
    }

    /// The property the repeat fixtures used to imply and never actually tested:
    /// the whole resolve/check/lower pipeline is a deterministic function of its
    /// source, down to the emitted module bytes.
    #[test]
    fn codegen_is_byte_deterministic() {
        const SOURCE: &str = "fn main(console: Console):\n    var xs = []\n    var i = 0\n    while i < 64:\n        xs.push(i)\n        i = i + 1\n    console.print(\"${xs.length()}\")\n";
        opt::set_for_tests(Some(OptSet::default_set()));
        let first = prepare(SOURCE).expect("first lowering");
        let second = prepare(SOURCE).expect("second lowering");
        opt::set_for_tests(None);
        assert_eq!(
            first.artifact.wasm(), second.artifact.wasm(),
            "lowering the same source twice must emit identical module bytes"
        );
    }

    /// RFC-0028 `for var`: a mutation of the loop element is written back into the
    /// list on admitted Wasm in both default and forced-copy modes.
    #[test]
    fn for_var_writes_elements_back_on_both_backends() {
        let src = "fn main(console: Console):\n    var xs = [1, 2, 3, 4]\n    for var x in xs:\n        x = x * 10\n    console.print(\"${xs}\")\n";
        let expected = ["[10, 20, 30, 40]".to_string()];
        opt::set_for_tests(Some(OptSet::default_set()));
        let wasm = compute(src).expect("wasm").output;
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::InPlace)));
        let wasm_copy = compute(src).expect("wasm -inplace").output;
        opt::set_for_tests(None);
        assert_eq!(wasm, expected, "for var: admitted Wasm must write elements back");
        assert_eq!(wasm_copy, expected, "for var: forced-copy must write elements back");
    }

    /// A frame-confined record built each iteration and read only through
    /// its fields leaves no heap behind: the heap stays flat as iterations grow.
    #[test]
    fn confined_record_loop_heap_stays_bounded() {
        let source = |iterations: usize| format!("type P:\n    x: Int\n    y: Int\nfn main(console: Console):\n    var total = 0\n    var i = 0\n    while i < {iterations}:\n        let p = P(i, i + 1)\n        total = total + p.x + p.y\n        i = i + 1\n    console.print(\"${{total}}\")\n");
        let small = compute(&source(300)).expect("compute 300 iterations");
        let large = compute(&source(3000)).expect("compute 3000 iterations");
        assert_eq!(small.output, vec!["90000".to_string()]);
        assert_eq!(large.output, vec!["9000000".to_string()]);
        assert!(
            large.heap_bytes <= small.heap_bytes + 64,
            "a confined record leaves no per-iteration heap: 300={} 3000={}",
            small.heap_bytes,
            large.heap_bytes
        );
    }

    /// The goal's NEVER-OOM property: a long loop with transient list/string
    /// scratch AND a helper that allocates internally but returns a scalar runs in
    /// constant heap regardless of iteration count.
    #[test]
    fn never_oom_long_loop_stays_bounded() {
        let source = |iterations: usize| format!("fn cost(n: Int) -> Int:\n    let tmp = [n, n + 1, n + 2]\n    list.length(tmp) + \"${{n}}\".length()\nfn main(console: Console):\n    var total = 0\n    var i = 0\n    while i < {iterations}:\n        let scratch = [i, i + 1]\n        total = total + cost(i) + list.length(scratch)\n        i = i + 1\n    console.print(\"${{total}}\")\n");
        let small = compute(&source(500)).expect("compute 500 iterations");
        let large = compute(&source(5000)).expect("compute 5000 iterations");
        assert_eq!(small.output, vec!["3890".to_string()]);
        assert_eq!(large.output, vec!["43890".to_string()]);
        assert_bounded_workload_growth(
            &small,
            &large,
            64,
            "never-OOM scratch loop must remain bounded",
        );
    }

    /// `for var` v1 rejects an early exit that belongs to the loop (it would skip
    /// the write-back), but allows a `continue` that belongs to a NESTED loop.
    #[test]
    fn for_var_rejects_loop_belonging_early_exit() {
        let bad = "fn main(console: Console):\n    var xs = [1, 2, 3]\n    for var x in xs:\n        if x == 2:\n            continue\n        x = x * 10\n    console.print(\"${xs}\")\n";
        assert!(crate::resolve_std_only(bad).is_err(), "a loop-belonging continue must be rejected");
        let ok = "fn main(console: Console):\n    var xs = [1, 2, 3]\n    for var x in xs:\n        for y in [0, 1]:\n            if y == 9:\n                continue\n        x = x * 10\n    console.print(\"${xs}\")\n";
        assert!(crate::resolve_std_only(ok).is_ok(), "a nested-loop continue must be allowed");
    }
}
