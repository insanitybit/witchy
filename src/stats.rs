//! RFC-0030: deterministic optimization counters (`witchy stats`).
//!
//! Compile a program and run it under the active [`crate::opt`] (`WITCHY_OPT`)
//! setting, returning exact operation *counts* — not timings — so an
//! optimization's effect is a unit-testable fact rather than a flaky benchmark.
//! A forced-copy run (`WITCHY_OPT=-inplace`) re-owns at every accumulation site
//! and allocates O(n^2) heap; an in-place run re-owns far fewer times and
//! allocates O(n). The differential sweep proves an optimization changes nothing
//! *observable*; these counters prove it actually *fired*.
#![cfg(feature = "native")]

use crate::runtime::{Capabilities, Runtime};
use crate::{codegen, typeck};

/// Wasm linear-memory pages for a stats run (mirrors the CLI's run budget).
const STATS_MEMORY_PAGES: usize = 16384;

/// Deterministic counters from one program run.
#[derive(Debug, Clone)]
pub struct Stats {
    /// The program's printed output — so a stats run also pins behavior.
    pub output: Vec<String>,
    /// Final `$heap` frontier in bytes (the peak, with no reclaim).
    pub heap_bytes: i64,
    /// Accumulation sites that entered with a zero ownership token — each copies.
    /// Small on a clean in-place run; ~one-per-iteration under `WITCHY_OPT=-inplace`.
    pub reowns: i64,
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
    pub bump_alloc_calls: i64,
    pub rc_reuse_calls: i64,
    pub rc_free_calls: i64,
    pub region_rewind_calls: i64,
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
}

/// Compile `src` (resolved against the bundled std) and run it under the active
/// `WITCHY_OPT` setting, returning its deterministic counters. The program must
/// need nothing beyond `Console` (the stats corpus is pure compute).
pub fn compute(src: &str) -> Result<Stats, String> {
    let linked = crate::resolve_std_only(src)?;
    typeck::check(&linked).map_err(|e| format!("{e:?}"))?;
    let bytes = match codegen::compile_module_binary(&linked) {
        codegen::LoweringOutcome::Lowered(bytes) => bytes,
        codegen::LoweringOutcome::Unsupported(reason) => return Err(reason.to_string()),
        codegen::LoweringOutcome::Rejected(error) => return Err(error.message),
    };
    let caps = Capabilities { print: true, print_int: true, quiet: true, ..Default::default() };
    let mut rt = Runtime::batch().map_err(|e| e.to_string())?;
    let mut vm = rt
        .spawn(&bytes, caps, STATS_MEMORY_PAGES)
        .map_err(|e| e.to_string())?;
    vm.run().map_err(|e| e.root_cause().to_string())?;
    Ok(Stats {
        output: vm.output(),
        heap_bytes: vm.heap_bytes().unwrap_or(0),
        reowns: vm.reowns().unwrap_or(0),
        region_copy_bytes: vm.region_copy_bytes().unwrap_or(0),
        rc_reused_bytes: vm.rc_reused_bytes().unwrap_or(0),
        live_cells: vm.live_cells().unwrap_or(0),
        rc_alloc_calls: vm.rc_alloc_calls().unwrap_or(0),
        bump_alloc_calls: vm.bump_alloc_calls().unwrap_or(0),
        rc_reuse_calls: vm.rc_reuse_calls().unwrap_or(0),
        rc_free_calls: vm.rc_free_calls().unwrap_or(0),
        region_rewind_calls: vm.region_rewind_calls().unwrap_or(0),
        extract_searches: vm.extract_searches().unwrap_or(0),
        extract_key_comparisons: vm.extract_key_comparisons().unwrap_or(0),
        extract_copied_bytes: vm.extract_copied_bytes().unwrap_or(0),
        extract_retains: vm.extract_retains().unwrap_or(0),
        extract_drops: vm.extract_drops().unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opt::{self, Opt, OptSet};

    // An accumulation loop: in-place push keeps it O(n); forced copy re-owns at
    // every iteration and balloons the heap to O(n^2).
    const ACC: &str = "fn main(console: Console):\n    var xs = []\n    var i = 0\n    while i < 400:\n        list.push(xs, i)\n        i = i + 1\n    console.print(\"${list.length(xs)}\")\n";

    // A soak loop: per-iteration scratch that never escapes. The `region`
    // (loop-watermark) reclaim resets the arena each iteration, so heap stays
    // CONSTANT no matter how many iterations; with it off the same program leaks.
    const SOAK: &str = "fn main(console: Console):\n    var sum = 0\n    var i = 0\n    while i < 5000:\n        let tmp = [i, i + 1, i + 2, i + 3]\n        sum = sum + list.length(tmp)\n        i = i + 1\n    console.print(\"${sum}\")\n";

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

    #[test]
    fn fip_depth_adds_no_allocation_or_reclamation_operations() {
        opt::set_for_tests(Some(OptSet::default_set()));
        let small_source = fip_kernel(8);
        let large_source = fip_kernel(50_000);

        let linked = crate::resolve_std_only(&large_source).expect("resolve FIP kernel");
        typeck::check(&linked).expect("type-check FIP kernel");
        assert!(
            crate::analysis::module_fip_misses(&linked).is_empty(),
            "the representative state machine must satisfy the static FIP contract"
        );

        let small = compute(&small_source).expect("run small FIP kernel");
        let large = compute(&large_source).expect("run large FIP kernel");
        opt::set_for_tests(None);

        assert_eq!(small.output, ["8"]);
        assert_eq!(large.output, ["50000"]);
        let resources = |stats: &Stats| {
            (
                stats.rc_alloc_calls,
                stats.bump_alloc_calls,
                stats.rc_reuse_calls,
                stats.rc_free_calls,
                stats.region_rewind_calls,
            )
        };
        assert_eq!(
            resources(&small),
            resources(&large),
            "recursive depth must add no allocator, reuse, free, or rewind operations"
        );
        assert_eq!(small.rc_reuse_calls, 0);
        assert_eq!(small.rc_free_calls, 0);
        assert_eq!(small.region_rewind_calls, 0);
        assert_eq!(
            (small.rc_alloc_calls, small.bump_alloc_calls),
            (4, 4),
            "the checked workload has four fixed setup allocations and none per transition"
        );
    }

    #[test]
    fn no_mutation_fip_kernel_returns_its_incoming_ownership_token() {
        const DIRECT: &str = "mode opt\n\ntype State:\n    count: Int\n\nfn main(console: Console):\n    var state = State(1)\n    state.count = 2\n    console.print(\"${state.count}\")\n";
        const FORWARDED: &str = "mode opt\n\ntype State:\n    count: Int\n\nfn forward(own state: unique State, n: Int) -> unique State:\n    if n == 0:\n        return state\n    forward(state, n - 1)\n\nfn main(console: Console):\n    var state = forward(State(1), 50000)\n    state.count = 2\n    console.print(\"${state.count}\")\n";

        opt::set_for_tests(Some(OptSet::default_set()));
        let direct = compute(DIRECT).expect("direct unique record update");
        let forwarded = compute(FORWARDED).expect("forwarded FIP owner update");
        opt::set_for_tests(None);

        assert_eq!(forwarded.output, direct.output);
        assert_eq!(
            (forwarded.rc_alloc_calls, forwarded.bump_alloc_calls),
            (direct.rc_alloc_calls, direct.bump_alloc_calls),
            "returning the untouched owner must preserve its token and avoid a re-own copy"
        );
    }

    /// RFC-0030 counter assertion: the `inplace` optimization is proven to FIRE
    /// (fewer re-owns, less heap) while changing nothing observable. This is the
    /// `(b)` half of the definition of done, the complement to the differential
    /// sweep's `(a)`.
    #[test]
    fn inplace_fires_and_is_measured() {
        opt::set_for_tests(Some(OptSet::default_set()));
        let on = compute(ACC).expect("compute with inplace on");
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::InPlace)));
        let off = compute(ACC).expect("compute with inplace off");
        opt::set_for_tests(None);

        // Same observable behavior (parity) ...
        assert_eq!(on.output, off.output, "the optimization must not change output");
        assert_eq!(on.output, vec!["400".to_string()]);
        // ... but the counters show it measurably fired. The in-place path copies
        // only to establish the first buffer (and on the ~log n growth steps),
        // never once per element — so its re-own count stays tiny. (`reowns` is the
        // in-place path's own counter; the forced-copy build never emits it, hence
        // 0 there — `heap_bytes` is the cross-setting proof.)
        assert!(on.reowns <= 2, "in-place accumulation barely re-owns: {}", on.reowns);
        // The allocation proof: in-place is O(n) where forced-copy is O(n^2).
        assert!(
            off.heap_bytes > on.heap_bytes * 4,
            "forced-copy must allocate far more heap: on={} off={}",
            on.heap_bytes,
            off.heap_bytes
        );
    }

    /// RFC-0088: result-bearing `var` calls carry the same hidden ownership
    /// token as statement mutators. Pop, replacing insert, and remove/insert
    /// cycles therefore reuse their container after the first CoW re-own.
    #[test]
    fn update_and_extract_threads_ownership_through_var_calls() {
        const EXTRACT: &str = "mode opt\n\nfn main(console: Console):\n    var xs = []\n    var i = 0\n    while i < 160:\n        xs.push(i)\n        i = i + 1\n    var sum = 0\n    while list.length(xs) > 0:\n        sum = sum + (xs.pop() ?? 0)\n\n    var d = dict.new()\n    i = 0\n    while i < 240:\n        let _ = d.insert(1, i)\n        i = i + 1\n    i = 0\n    while i < 160:\n        let _ = d.remove(1)\n        let _ = d.insert(1, i)\n        i = i + 1\n    console.print(\"${sum}\")\n    console.print(\"${dict.at(d, 1)}\")\n";

        let linked = crate::resolve_std_only(EXTRACT).expect("resolve opt extraction workload");
        typeck::check(&linked).expect("type-check opt extraction workload");
        let misses: Vec<_> = crate::analysis::module_no_copy_misses(&linked)
            .into_iter()
            .filter(|miss| miss.function == "main")
            .collect();
        assert!(misses.is_empty(), "the measured unique loops satisfy opt mode: {misses:?}");

        opt::set_for_tests(Some(OptSet::default_set()));
        let on = compute(EXTRACT).expect("ownership-aware extraction");
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::InPlace)));
        let off = compute(EXTRACT).expect("forced-copy extraction");
        opt::set_for_tests(None);

        assert_eq!(on.output, off.output, "ownership lowering must preserve values");
        assert_eq!(on.output, vec!["12720".to_string(), "159".to_string()]);
        assert_eq!(on.extract_searches, 560, "each insert/remove performs one search");
        assert_eq!(off.extract_searches, 560, "forced-copy keeps one search per operation");
        assert!(
            on.extract_copied_bytes * 20 < off.extract_copied_bytes,
            "unique extraction must avoid full-container copies: on={} off={}",
            on.extract_copied_bytes,
            off.extract_copied_bytes
        );
        assert!(
            off.heap_bytes > on.heap_bytes * 4,
            "forced-copy extraction must allocate materially more: on={} off={}",
            on.heap_bytes,
            off.heap_bytes
        );
    }

    #[test]
    fn discarded_var_result_does_not_overwrite_the_receiver() {
        const BARE: &str = "fn main(console: Console):\n    var d = dict.new()\n    d.insert(1, 7)\n    d.insert(2, 9)\n    d.remove(1)\n    console.print(\"${dict.at(d, 2)}\")\n    console.print(\"${dict.length(d)}\")\n";

        opt::set_for_tests(Some(OptSet::default_set()));
        let on = compute(BARE).expect("bare var calls with inplace on");
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::InPlace)));
        let off = compute(BARE).expect("bare var calls with forced copy");
        opt::set_for_tests(None);

        assert_eq!(on.output, ["9", "1"]);
        assert_eq!(off.output, on.output, "discarding Option must not replace the Dict");
    }

    #[test]
    fn unique_result_threads_capacity_into_extraction() {
        const UNIQUE_RESULT: &str = "mode opt\n\nimport list\n\nfn build() -> unique List(Int):\n    [1, 2, 3]\n\nfn forward() -> unique List(Int):\n    build()\n\nfn main(console: Console):\n    var xs = forward()\n    console.print(\"${xs.pop() ?? 0}\")\n    console.print(\"${xs.length()}\")\n";

        let linked = crate::resolve_std_only(UNIQUE_RESULT).expect("resolve unique result");
        typeck::check(&linked).expect("type-check unique result");
        let misses: Vec<_> = crate::analysis::module_no_copy_misses(&linked)
            .into_iter()
            .filter(|miss| miss.function == "main")
            .collect();
        assert!(misses.is_empty(), "the result qualifier supplies the proof: {misses:?}");

        let stats = compute(UNIQUE_RESULT).expect("run unique-result extraction");
        assert_eq!(stats.output, ["3", "2"]);
        assert_eq!(stats.extract_copied_bytes, 0, "the returned cap must prevent a copy");
        assert_eq!(stats.reowns, 0, "the caller must receive the producer's token");

        const NORMAL_INDIRECT: &str = "fn build() -> unique List(Int):\n    [1, 2]\n\nfn invoke(f: fn() -> unique List(Int)) -> List(Int):\n    f()\n\nfn main(console: Console):\n    let xs = invoke(build)\n    console.print(\"${list.length(xs)}\")\n";
        let indirect = compute(NORMAL_INDIRECT).expect("normal-mode first-class unique result");
        assert_eq!(indirect.output, ["2"], "discarding the hidden token preserves values");

        const ASSIGNED: &str = "mode opt\n\nimport list\n\nfn build() -> unique List(Int):\n    [1, 2, 3]\n\nfn main(console: Console):\n    var xs = [0]\n    xs = build()\n    console.print(\"${xs.pop() ?? 0}\")\n";
        let assigned = compute(ASSIGNED).expect("assigned unique-result extraction");
        assert_eq!(assigned.output, ["3"]);
        assert_eq!(assigned.extract_copied_bytes, 0, "assignment must preserve the returned token");
        assert_eq!(assigned.reowns, 0);

        const METHOD: &str = "mode opt\n\nimport list\n\ntype Builder:\n    seed: Int\n\nimpl Builder:\n    fn build(let self: Builder) -> unique List(Int):\n        [self.seed, 9]\n\nfn main(console: Console):\n    let builder = Builder(4)\n    var xs = builder.build()\n    console.print(\"${xs.pop() ?? 0}\")\n";
        let method = compute(METHOD).expect("direct unique-result method extraction");
        assert_eq!(method.output, ["9"]);
        assert_eq!(method.extract_copied_bytes, 0);
        assert_eq!(method.reowns, 0);

        const WITH_VAR: &str = "mode opt\n\nimport list\n\nfn split(var xs: unique List(Int)) -> unique List(Int):\n    xs.push(8)\n    [1, 2]\n\nfn main(console: Console):\n    var xs = [4]\n    var ys = split(xs)\n    console.print(\"${ys.pop() ?? 0}\")\n    console.print(\"${xs.pop() ?? 0}\")\n";
        let with_var = compute(WITH_VAR).expect("unique result plus collection var write-back");
        assert_eq!(with_var.output, ["2", "8"]);
        assert_eq!(with_var.extract_copied_bytes, 0);
        assert_eq!(with_var.reowns, 0);
    }

    #[test]
    fn unique_result_early_return_precedes_var_writeback() {
        const EARLY: &str = "mode opt\n\nimport list\n\nfn choose(var n: Int, flag: Bool) -> unique List(Int):\n    n = n + 1\n    if flag:\n        return [1, 2, 3]\n    [4, 5]\n\nfn main(console: Console):\n    var n = 0\n    var xs = choose(n, true)\n    console.print(\"${xs.pop() ?? 0}\")\n    console.print(\"${n}\")\n";

        let linked = crate::resolve_std_only(EARLY).expect("resolve early unique result");
        typeck::check(&linked).expect("type-check early unique result");
        let misses: Vec<_> = crate::analysis::module_no_copy_misses(&linked)
            .into_iter()
            .filter(|miss| miss.function == "main")
            .collect();
        assert!(misses.is_empty(), "the early result still carries its proof: {misses:?}");

        let stats = compute(EARLY).expect("run early unique-result extraction");
        assert_eq!(stats.output, ["3", "1"]);
        assert_eq!(stats.extract_copied_bytes, 0);
        assert_eq!(stats.reowns, 0);

        const ALL_RETURN: &str = "mode opt\n\nimport list\n\nfn choose(flag: Bool) -> unique List(Int):\n    if flag:\n        return [1]\n    return [2, 3]\n\nfn main(console: Console):\n    var xs = choose(false)\n    console.print(\"${xs.pop() ?? 0}\")\n";
        let all_return = compute(ALL_RETURN).expect("run exhaustive explicit unique returns");
        assert_eq!(all_return.output, ["3"]);
        assert_eq!(all_return.extract_copied_bytes, 0);
        assert_eq!(all_return.reowns, 0);
    }

    /// Heap leaves make ownership traffic observable. The unique path moves
    /// displaced values without retaining them; forced copy must retain both
    /// the returned projection and every leaf kept by the repaired container.
    #[test]
    fn update_and_extract_counts_heap_leaf_ownership() {
        const HEAP_LEAVES: &str = r#"import dict

fn heap(s: String) -> String:
    s + "!"

fn main(console: Console):
    var xs = [heap("a"), heap("b")]
    let popped = xs.pop()
    var d = dict.new()
    let _ = d.insert(heap("k"), heap("old"))
    let replaced = d.insert(heap("k"), heap("new"))
    let removed = d.remove(heap("k"))
    console.print("${popped ?? "none"}")
    console.print("${replaced ?? "none"}")
    console.print("${removed ?? "none"}")
"#;

        opt::set_for_tests(Some(OptSet::default_set()));
        let on = compute(HEAP_LEAVES).expect("unique heap extraction");
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::InPlace)));
        let off = compute(HEAP_LEAVES).expect("shared heap extraction");
        opt::set_for_tests(None);

        assert_eq!(on.output, off.output, "ownership traffic must not change values");
        assert_eq!(on.output, vec!["b!", "old!", "new!"]);
        assert_eq!(on.extract_searches, 3);
        assert_eq!(off.extract_searches, 3);
        assert_eq!(on.extract_retains, 3, "only newly stored heap leaves retain");
        assert_eq!(off.extract_retains, 9, "CoW retains returned and copied leaves");
        assert_eq!(on.extract_drops, 1, "unique removal releases its abandoned key");
        assert_eq!(off.extract_drops, 1, "CoW replacement balances its copied old leaf");
        assert_eq!(on.extract_copied_bytes, 4, "only the initial empty dict re-owns");
        assert_eq!(off.extract_copied_bytes, 32, "CoW copies list and dict structure");
    }

    /// Empty pop and missing removal are true no-op repairs: no allocation,
    /// structural copy, retain, or drop is permitted.
    #[test]
    fn update_and_extract_misses_do_no_ownership_work() {
        const MISSES: &str = r#"import dict

fn main(console: Console):
    var xs: List(String) = []
    let popped = xs.pop()
    var d = dict.from_pairs([("a", "one")])
    let removed = d.remove("missing")
    console.print("${popped ?? "none"}")
    console.print("${removed ?? "none"}")
    console.print("${dict.at(d, "a")}")
"#;

        opt::set_for_tests(Some(OptSet::default_set()));
        let stats = compute(MISSES).expect("missing extraction");
        opt::set_for_tests(None);

        assert_eq!(stats.output, vec!["none", "none", "one"]);
        assert_eq!(stats.extract_searches, 1);
        assert_eq!(stats.extract_copied_bytes, 0);
        assert_eq!(stats.extract_retains, 0);
        assert_eq!(stats.extract_drops, 0);
    }

    /// Geometric growth plus index maintenance keeps missing insertion and
    /// replacing insertion amortized, while every call still performs exactly
    /// one semantic search.
    #[test]
    fn update_and_extract_preserves_indexed_dict_growth() {
        const INDEXED: &str = r#"import dict

fn main(console: Console):
    var d = dict.new()
    var i = 0
    while i < 256:
        let _ = d.insert(i, i)
        i = i + 1
    i = 0
    while i < 200:
        let _ = d.insert(128, i)
        i = i + 1
    console.print("${dict.length(d)}")
    console.print("${dict.at(d, 128)}")
"#;

        opt::set_for_tests(Some(OptSet::default_set()));
        let stats = compute(INDEXED).expect("indexed extraction growth");
        opt::set_for_tests(None);

        assert_eq!(stats.output, vec!["256", "199"]);
        assert_eq!(stats.extract_searches, 456);
        assert!(
            stats.extract_key_comparisons < stats.extract_searches * 4,
            "hash-index probes must stay bounded: searches={} comparisons={}",
            stats.extract_searches,
            stats.extract_key_comparisons
        );
        assert!(
            stats.extract_copied_bytes < 16_000,
            "geometric growth must avoid per-insert full copies: {} bytes",
            stats.extract_copied_bytes
        );
    }

    /// Rebinding a capacity-bearing `var` parameter replaces its allocation.
    /// The old token must be cleared before the next append; otherwise a large
    /// caller capacity authorizes an out-of-bounds write into the new empty list.
    #[test]
    fn var_collection_rebind_resets_the_capacity_token() {
        const REBIND: &str = r#"fn reset(var xs: List(Int)) -> Nil:
    xs = []
    xs.push(9)

fn main(console: Console):
    var xs = []
    var i = 0
    while i < 64:
        xs.push(i)
        i = i + 1
    reset(xs)
    console.print("${xs}")
"#;

        opt::set_for_tests(Some(OptSet::default_set()));
        let stats = compute(REBIND).expect("collection var rebind");
        opt::set_for_tests(None);

        assert_eq!(stats.output, vec!["[9]"]);
        assert_eq!(
            stats.reowns, 2,
            "the caller and rebound empty list must each establish ownership once"
        );
    }

    /// Missing shared removal must leave the old hash index untouched, and a
    /// successful unique removal must rebuild it after insertion-order repair.
    /// Both cases keep the following lookup on the indexed path.
    #[test]
    fn dict_remove_preserves_shared_storage_and_indexed_followups() {
        let pairs = (0..256)
            .map(|i| format!("({i}, {i})"))
            .collect::<Vec<_>>()
            .join(", ");
        let src = format!(
            "import dict\n\nfn main(console: Console):\n    var d = dict.from_pairs([{pairs}])\n    let snapshot = d\n    let _ = d.remove(-1)\n    let _ = d.insert(200, 900)\n    let _ = d.remove(0)\n    let _ = d.insert(199, 800)\n    console.print(\"${{dict.at(snapshot, 200)}}\")\n    console.print(\"${{dict.at(d, 200)}}\")\n    console.print(\"${{dict.at(d, 199)}}\")\n"
        );

        opt::set_for_tests(Some(OptSet::default_set()));
        let stats = compute(&src).expect("indexed remove followups");
        opt::set_for_tests(None);

        assert_eq!(stats.output, vec!["200", "900", "800"]);
        assert_eq!(stats.extract_searches, 4);
        assert!(
            stats.extract_key_comparisons < 24,
            "remove must not demote following lookups to a linear scan: {} comparisons",
            stats.extract_key_comparisons
        );
    }

    /// User equality can itself perform extraction. The instrumentation scope
    /// is a nesting depth, so the inner search cannot disable comparison counts
    /// for the remainder of the outer search.
    #[test]
    fn extraction_comparison_count_is_reentrant() {
        const NESTED_EQ: &str = r#"import dict

type Key:
    value: Int

impl PartialEq for Key:
    fn eq(self, other: Key) -> Bool:
        var probe = dict.from_pairs([(1, 1)])
        let _ = probe.remove(1)
        self.value == other.value

impl Eq for Key

fn main(console: Console):
    var d = dict.from_pairs([(Key(1), 10), (Key(2), 20)])
    let old = d.remove(Key(2))
    console.print("${old ?? -1}")
"#;

        opt::set_for_tests(Some(OptSet::default_set()));
        let stats = compute(NESTED_EQ).expect("nested extraction in Eq");
        opt::set_for_tests(None);

        assert_eq!(stats.output, vec!["20"]);
        assert_eq!(stats.extract_searches, 4);
        assert_eq!(
            stats.extract_key_comparisons, 5,
            "inner extraction must preserve the outer comparison scope"
        );
    }

    /// (RFC-0033 R1) A record whose field is updated in a loop AND which escapes
    /// (returned → a heap record, not an SROA-confined one) must update IN PLACE
    /// when uniquely owned: O(1) heap, not a fresh record per iteration. Output is
    /// invariant; `heap_bytes` is the proof the per-update realloc is gone. This
    /// threads the in-place optimization through a user type — the edge RFC-0033
    /// closes (without it, `s.field = v` on an escaping record was O(n) reallocs).
    #[test]
    fn record_field_update_is_in_place() {
        const REC: &str = "type Counter:\n    count: Int\n    pad: Int\n\nfn build(n: Int) -> Counter:\n    var c = Counter(0, 0)\n    var i = 0\n    while i < n:\n        c.count = c.count + 1\n        i = i + 1\n    c\n\nfn main(console: Console):\n    console.print(\"${build(400).count}\")\n";
        opt::set_for_tests(Some(OptSet::default_set()));
        let on = compute(REC).expect("compute with inplace on");
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::InPlace)));
        let off = compute(REC).expect("compute with inplace off");
        opt::set_for_tests(None);

        assert_eq!(on.output, off.output, "in-place must not change output");
        assert_eq!(on.output, vec!["400".to_string()]);
        assert!(
            off.heap_bytes > on.heap_bytes * 4,
            "forced-copy reallocs a record per field update: on={} off={}",
            on.heap_bytes,
            off.heap_bytes
        );
    }

    /// (RFC-0033 R3) own-ABI threads through a USER record type: `c = bump(c)`,
    /// where `bump(own c: Counter)` mutates a field and returns it, keeps the
    /// record's ownership ACROSS the call boundary, so the loop reuses one record
    /// (O(1) heap) instead of reallocating per call. This is the edge RFC-0033
    /// closes end to end — in-place threading compounding through a user function.
    #[test]
    fn record_own_abi_threads_in_place() {
        const REC: &str = "type Counter:\n    count: Int\n    pad: Int\n\nfn bump(own c: Counter) -> Counter:\n    c.count = c.count + 1\n    c\n\nfn build(n: Int) -> Counter:\n    var c = Counter(0, 0)\n    var i = 0\n    while i < n:\n        c = bump(c)\n        i = i + 1\n    c\n\nfn main(console: Console):\n    console.print(\"${build(400).count}\")\n";
        opt::set_for_tests(Some(OptSet::default_set()));
        let on = compute(REC).expect("on");
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::InPlace)));
        let off = compute(REC).expect("off");
        opt::set_for_tests(None);
        assert_eq!(on.output, off.output, "own-ABI threading must not change output");
        assert_eq!(on.output, vec!["400".to_string()]);
        assert!(
            off.heap_bytes > on.heap_bytes * 4,
            "own-ABI must thread the record in place across the call: on={} off={}",
            on.heap_bytes,
            off.heap_bytes
        );
    }

    /// (RFC-0033 R2) A record FIELD that is a list, grown with
    /// `s.items = list.push(s.items, x)` in a loop, must grow the field's list
    /// buffer IN PLACE rather than copying the whole field each update. Output is
    /// invariant; `heap_bytes` is the proof the per-update O(n) copy is gone (O(n)
    /// total vs O(n^2)). The field is read nowhere but the push receiver, so the
    /// buffer is never aliased — the field-push-safe gate lets R2 fire.
    #[test]
    fn record_field_list_push_is_in_place() {
        const REC: &str = "type Stack:\n    items: List(Int)\n    size: Int\n\nfn build(n: Int) -> Stack:\n    var s = Stack([], 0)\n    var i = 0\n    while i < n:\n        list.push(s.items, i)\n        i = i + 1\n    s\n\nfn main(console: Console):\n    console.print(\"${list.length(build(200).items)}\")\n";
        opt::set_for_tests(Some(OptSet::default_set()));
        let on = compute(REC).expect("on");
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::InPlace)));
        let off = compute(REC).expect("off");
        opt::set_for_tests(None);
        assert_eq!(on.output, off.output, "in-place field push must not change output");
        assert_eq!(on.output, vec!["200".to_string()]);
        assert!(
            off.heap_bytes > on.heap_bytes * 4,
            "forced-copy reallocs the field list per push: on={} off={}",
            on.heap_bytes,
            off.heap_bytes
        );
    }

    /// RFC-0030 soak / never-OOM guard: a long loop whose per-iteration scratch is
    /// escape-free must run in BOUNDED heap — the `region` loop-watermark reclaim
    /// resets the arena every iteration. With it off the same program leaks O(n).
    /// `peak_heap < budget` is the deterministic never-OOM assertion the goal names.
    #[test]
    fn region_reclaim_keeps_soak_bounded() {
        opt::set_for_tests(Some(OptSet::default_set()));
        let on = compute(SOAK).expect("compute with region on");
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::Region)));
        let off = compute(SOAK).expect("compute with region off");
        opt::set_for_tests(None);

        assert_eq!(on.output, off.output, "the optimization must not change output");
        assert_eq!(on.output, vec!["20000".to_string()]);
        assert_eq!(on.region_rewind_calls, 5000, "one rewind per completed iteration");
        assert_eq!(off.region_rewind_calls, 0, "disabled region lowering emits no rewinds");
        // Bounded heap regardless of iteration count — the never-OOM floor.
        assert!(on.heap_bytes < 4096, "region reclaim must bound the soak heap: {}", on.heap_bytes);
        // ... and far below the leaking build.
        assert!(
            off.heap_bytes > on.heap_bytes * 10,
            "without region the soak leaks O(n): on={} off={}",
            on.heap_bytes,
            off.heap_bytes
        );
    }

    /// RFC-0030 differential de-opt sweep (DoD half (a)): a program's OUTPUT must
    /// be identical under every `WITCHY_OPT` setting — `none`, `all`, the default,
    /// and the default minus each optimization — AND must match the interpreter
    /// oracle. Toggling an optimization changes *how* a program runs, never *what*
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
            "fn main(console: Console):\n    var xs = []\n    var s = \"\"\n    var d = dict.new()\n    var i = 0\n    while i < 300:\n        let scratch = [i, i + 1]\n        xs.push(i + list.length(scratch) - 2)\n        s = s + \"${i % 10}\"\n        dict.update(d, i % 7, 0, fn(n: Int): n + 1)\n        i = i + 1\n    let folded = 2 * 3 + 4\n    console.print(\"${list.length(xs)}\")\n    console.print(\"${string.length(s)}\")\n    console.print(\"${dict.get_or(d, 3, 0)}\")\n    console.print(\"${folded}\")\n",
            // `for var` write-back over record elements.
            "type P:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    var ps = [P(1, 2), P(3, 4)]\n    for var p in ps:\n        p.x = p.x + 100\n    console.print(\"${ps}\")\n",
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
            // shared record in place. The interpreter oracle pins "1 99"; an unsound
            // in-place would print "99 99" and fail this differential.
            "type R:\n    a: Int\n    b: Int\n\nfn main(console: Console):\n    var r = R(1, 2)\n    let alias = r\n    r.a = 99\n    console.print(\"${alias.a} ${r.a}\")\n",
            // (RFC-0033 R3) own-ABI threading through a user record + a PLAIN call
            // to an own-ABI fn (`id(Counter(7,0))`) — both must be invariant under
            // toggling `inplace` and match the interpreter oracle.
            "type Counter:\n    count: Int\n    pad: Int\n\nfn bump(own c: Counter) -> Counter:\n    c.count = c.count + 1\n    c\n\nfn id(own c: Counter) -> Counter:\n    c\n\nfn build(n: Int) -> Counter:\n    var c = Counter(0, 0)\n    var i = 0\n    while i < n:\n        c = bump(c)\n        i = i + 1\n    c\n\nfn main(console: Console):\n    console.print(\"${build(30).count}\")\n    console.print(\"${id(Counter(7, 0)).count}\")\n",
            // CACHE EVICTION: insert then remove distinct dict keys (the per-object
            // RC floor's target garbage). Output must stay invariant under every
            // `WITCHY_OPT` setting and match the interpreter — the parity guard for
            // the residual the floor will eventually bound (see
            // `cache_eviction_leaks_without_rc_floor`).
            "import dict\n\nfn main(console: Console):\n    var d = dict.new()\n    var i = 0\n    while i < 40:\n        dict.insert(d, i, i * 2)\n        dict.remove(d, i)\n        i = i + 1\n    console.print(\"${dict.length(d)}\")\n    dict.insert(d, 7, 70)\n    console.print(\"${dict.get_or(d, 7, 0)}\")\n",
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
            // and diverge once `f` is rebound). The interpreter oracle pins both.
            "fn main(console: Console):\n    let k = 10\n    let g = fn(x: Int): x + k\n    var f = fn(x: Int): x * 2\n    var i = 0\n    var acc = 0\n    while i < 8:\n        acc = acc + g(i) + f(i)\n        if i == 4:\n            f = fn(x: Int): x * 3\n        i = i + 1\n    console.print(\"${acc}\")\n",
            // (RFC-0034 L2) Bounds-check elision: `for i in 0..list.length(xs)` indexing
            // an unmutated `xs` lowers `xs[i]` to an UNCHECKED load (provably in range);
            // toggling `bounds-elide` swaps checked/unchecked codegen and must not change
            // output. Two elidable loops over distinct lists; the interpreter (always
            // bounds-checked) is the oracle, so an unsound elision reading out of range
            // would diverge here.
            "fn main(console: Console):\n    let xs = [3, 1, 4, 1, 5, 9, 2, 6]\n    let ys = [10, 20]\n    var t = 0\n    for i in 0..list.length(xs):\n        t = t + xs[i] * i\n    for j in 0..list.length(ys):\n        t = t + ys[j]\n    console.print(\"${t}\")\n",
        ];
        std::thread::scope(|s| {
            let handles: Vec<_> = corpus.iter().map(|src| {
                s.spawn(move || {
                    let linked = crate::resolve_std_only(src).expect("link");
                    typeck::check(&linked).expect("typeck");
                    let oracle =
                        crate::interpreter::run_module(linked, ".", Vec::new()).expect("interp run");

                    opt::set_for_tests(Some(OptSet::all()));
                    let base = compute(src).expect("compute all").output;
                    opt::set_for_tests(None);
                    assert_eq!(base, oracle, "wasm (all) must match the interpreter oracle for:\n{src}");

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

    fn interp(src: &str) -> Vec<String> {
        let linked = crate::resolve_std_only(src).expect("link");
        typeck::check(&linked).expect("typeck");
        crate::interpreter::run_module(linked, ".", Vec::new()).expect("interp run")
    }

    /// RFC-0028 confined Views DoD counter (b): a read-only window over an
    /// unmutated source, read only via `at`/`length`, elides the `list.slice`
    /// COPY — so reading a 400-element window allocates the source once with
    /// `views` on, and the source PLUS a full copy with it off. Output is
    /// identical (parity), and `heap_bytes` proves the copy is gone. The source
    /// is a parameter because a push-built list counts as reassigned.
    #[test]
    fn confined_view_elides_the_slice_copy() {
        // `xs` is a 400-element param; `win` slices the whole thing and reads it
        // only via `length`/`at`. The window copy is ~400*8 bytes.
        let src = "import list\n\nfn win(xs: List(Int)) -> Int:\n    let w = list.slice(xs, 0, 400)\n    var t = 0\n    var j = 0\n    while j < list.length(w):\n        t = t + list.at(w, j)\n        j = j + 1\n    t\n\nfn main(console: Console):\n    var xs = []\n    var i = 0\n    while i < 400:\n        list.push(xs, i)\n        i = i + 1\n    console.print(\"${win(xs)}\")\n";
        opt::set_for_tests(Some(OptSet::default_set()));
        let on = compute(src).expect("views on");
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::Views)));
        let off = compute(src).expect("views off");
        opt::set_for_tests(None);

        // Same observable behavior (sum 0..399 = 79800).
        assert_eq!(on.output, off.output, "views must not change output");
        assert_eq!(on.output, vec!["79800".to_string()]);
        // The copy is elided: views-off allocates a full ~3.2KB window copy on top
        // of the source that views-on never does.
        assert!(
            off.heap_bytes >= on.heap_bytes + 3000,
            "views must elide the slice copy: on={} off={}",
            on.heap_bytes,
            off.heap_bytes
        );
    }

    /// RFC-0016 RC-floor reuse — NON-uniform (capacity-resizing) case. A list `var`
    /// reassigned to VARYING lengths (`[0]` then `[i,i+1,i+2]`) is now bounded too:
    /// codegen capacity-checks at runtime, reusing the buffer when the new length
    /// fits and reallocating (ratcheting the buffer up to the max length) otherwise.
    /// So a build-and-drop loop with a non-uniform initial stays O(1) heap after the
    /// buffer warms up to the max length — it no longer leaks O(n). (Pathologically
    /// OSCILLATING lengths can still re-allocate, since the header tracks length not
    /// a separate capacity; that residual is the full per-object RC floor's job.)
    #[test]
    fn nonuniform_reassignment_is_capacity_resized_and_bounded() {
        let prog = |n: i32| {
            format!(
                "fn main(console: Console):\n    var latest = [0]\n    var i = 0\n    while i < {n}:\n        latest = [i, i + 1, i + 2]\n        i = i + 1\n    console.print(\"${{list.length(latest)}}\")\n"
            )
        };
        opt::set_for_tests(Some(OptSet::default_set()));
        let small = compute(&prog(500)).expect("n=500");
        let big = compute(&prog(3000)).expect("n=3000");
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::RcElide)));
        let off_big = compute(&prog(3000)).expect("off n=3000");
        opt::set_for_tests(None);
        assert_eq!(small.output, vec!["3".to_string()]);
        assert_eq!(big.output, off_big.output, "rc-elide must not change output");
        assert_eq!(big.output, vec!["3".to_string()]);
        // Bounded: after the buffer ratchets to length 3 (one realloc), every further
        // iteration reuses it — so 6× the iterations is ~the same heap.
        assert!(
            big.heap_bytes < small.heap_bytes * 2,
            "capacity-resizing reuse must bound the loop: n=500 heap={}, n=3000 heap={}",
            small.heap_bytes,
            big.heap_bytes
        );
        // ... and far below the leaking (rc-elide off) build.
        assert!(
            off_big.heap_bytes > big.heap_bytes * 2,
            "without rc-elide the non-uniform loop leaks O(n): on={} off={}",
            big.heap_bytes,
            off_big.heap_bytes
        );
    }

    /// RFC-0016 RC-floor reuse DoD (b): a confined, never-aliased `var` reassigned
    /// to SAME-LENGTH list literals each iteration overwrites its buffer in place
    /// (rc-elide on) instead of allocating a fresh list each time (off). So the
    /// build-and-drop loop is O(1) heap with the optimization and O(n) without —
    /// the never-OOM property for the uniform-reassignment case. Output identical.
    #[test]
    fn uniform_reassignment_is_reused_and_bounded() {
        let prog = |n: i32| {
            format!(
                "fn main(console: Console):\n    var latest = [0, 0, 0]\n    var i = 0\n    while i < {n}:\n        latest = [i, i + 1, i + 2]\n        i = i + 1\n    console.print(\"${{list.at(latest, 0) + list.at(latest, 2) + list.length(latest)}}\")\n"
            )
        };
        // rc-elide ON (default): heap is bounded regardless of iteration count.
        opt::set_for_tests(Some(OptSet::default_set()));
        let on_small = compute(&prog(500)).expect("on n=500");
        let on_big = compute(&prog(3000)).expect("on n=3000");
        // rc-elide OFF: the same loop leaks O(n) (a fresh list every iteration).
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::RcElide)));
        let off_big = compute(&prog(3000)).expect("off n=3000");
        opt::set_for_tests(None);

        // last iter: latest = [n-1, n, n+1]; at(0)+at(2)+len = (n-1)+(n+1)+3 = 2n+3.
        assert_eq!(on_big.output, off_big.output, "rc-elide must not change output");
        assert_eq!(on_big.output, vec![(2 * 3000 + 3).to_string()]);
        // Bounded: 6× the iterations is ~the same heap with reuse on.
        assert!(
            on_big.heap_bytes < on_small.heap_bytes * 2,
            "rc-elide must bound the build-and-drop loop: n=500 heap={}, n=3000 heap={}",
            on_small.heap_bytes,
            on_big.heap_bytes
        );
        // ... and far below the leaking (rc-elide off) build at the same count.
        assert!(
            off_big.heap_bytes > on_big.heap_bytes * 2,
            "without rc-elide the same loop leaks O(n): on={} off={}",
            on_big.heap_bytes,
            off_big.heap_bytes
        );
    }

    /// RFC-0016 RC-floor reuse — RECORD case. A confined `var` reassigned to the
    /// SAME constructor each iteration (fixed tag + arity → uniform by construction)
    /// overwrites its field slots in place instead of allocating a fresh record, so
    /// a whole-record-reassignment loop is O(1) heap (vs O(n) off). Output identical.
    #[test]
    fn record_reassignment_is_reused_and_bounded() {
        let prog = |n: i32| {
            format!(
                "type Point:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    var p = Point(0, 0)\n    var i = 0\n    while i < {n}:\n        p = Point(i, i * 2)\n        i = i + 1\n    console.print(\"${{p.x + p.y}}\")\n"
            )
        };
        opt::set_for_tests(Some(OptSet::default_set()));
        let on_small = compute(&prog(500)).expect("on n=500");
        let on_big = compute(&prog(3000)).expect("on n=3000");
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::RcElide)));
        let off_big = compute(&prog(3000)).expect("off n=3000");
        opt::set_for_tests(None);

        // last iter p = Point(n-1, (n-1)*2); x+y = (n-1)*3.
        assert_eq!(on_big.output, off_big.output, "rc-elide must not change output");
        assert_eq!(on_big.output, vec![((3000 - 1) * 3).to_string()]);
        assert!(
            on_big.heap_bytes < on_small.heap_bytes * 2,
            "record reuse must bound the loop: n=500 heap={}, n=3000 heap={}",
            on_small.heap_bytes,
            on_big.heap_bytes
        );
        assert!(
            off_big.heap_bytes > on_big.heap_bytes * 2,
            "without rc-elide the record loop leaks O(n): on={} off={}",
            on_big.heap_bytes,
            off_big.heap_bytes
        );
    }

    /// RFC-0016 never-OOM BOUNDARY (the complement of the leak above): the realistic
    /// server-cache case — a long loop overwriting a BOUNDED key set in a dict — is
    /// ALREADY bounded by in-place dict mutation (the buffer is reused, not regrown),
    /// independent of iteration count. This proves RC floor is NOT needed for the
    /// bounded-working-set cache; its remaining job is specifically ESCAPING garbage
    /// (the reassignment leak), not steady-state mutation. Pinning this keeps the
    /// never-OOM frontier precise: what's handled vs. what 0016 must still fix.
    #[test]
    fn bounded_keyset_dict_cache_is_already_bounded() {
        let prog = |n: i32| {
            format!(
                "import dict\n\nfn main(console: Console):\n    var d = dict.new()\n    var i = 0\n    while i < {n}:\n        dict.insert(d, i % 8, i)\n        i = i + 1\n    console.print(\"${{dict.length(d)}}\")\n"
            )
        };
        opt::set_for_tests(Some(OptSet::default_set()));
        let small = compute(&prog(500)).expect("n=500");
        let big = compute(&prog(3000)).expect("n=3000");
        opt::set_for_tests(None);
        assert_eq!(small.output, vec!["8".to_string()]);
        assert_eq!(big.output, vec!["8".to_string()]);
        // 6× the iterations over the same 8 keys → ~constant heap (in-place reuse),
        // unlike the escaping-reassignment leak. This is the never-OOM property
        // HOLDING for the bounded-working-set case.
        assert!(
            big.heap_bytes < small.heap_bytes * 2,
            "bounded-keyset dict cache must stay bounded (in-place), but heap scaled \
             with iterations: n=500 heap={}, n=3000 heap={}",
            small.heap_bytes,
            big.heap_bytes
        );
    }

    /// RFC-0016 never-OOM RESIDUAL (the pin the per-object RC floor must clear): a
    /// cache-EVICTION loop — insert then remove DISTINCT keys in a dict — leaks O(n)
    /// today. Unlike `bounded_keyset_dict_cache_is_already_bounded` (a bounded key
    /// set, kept O(1) by in-place dict mutation), eviction churns: every
    /// `dict.insert` of a fresh key and every `dict.remove` allocates a new dict
    /// buffer, and the old buffer — though dead and uniquely owned — is never
    /// reclaimed by the watermark (the dict escapes the iteration) nor by the reuse
    /// rung (reassignment to a builtin result, not a same-shape literal). So heap
    /// grows with iteration count. This is EXACTLY the generally-escaping garbage
    /// RFC-0016's per-object refcount floor (dec-at-last-use + size-classed free
    /// list) targets. The assertion pins the leak so it FLIPS (green→needs-update)
    /// when the floor lands and bounds it — the DoD (b) characterization for the
    /// floor, the inverse of the bounded-keyset pin.
    #[test]
    fn cache_eviction_bounded_by_rc_floor() {
        let prog = |n: i32| {
            format!(
                "import dict\n\nfn main(console: Console):\n    var d = dict.new()\n    var i = 0\n    while i < {n}:\n        dict.insert(d, i, i)\n        dict.remove(d, i)\n        i = i + 1\n    console.print(\"${{dict.length(d)}}\")\n"
            )
        };
        // RC-floor OFF (the opt-in lever absent): the eviction garbage leaks O(n).
        // Every `dict.remove` allocates a fresh buffer and the old, dead, uniquely
        // owned one is never reclaimed — so 6× the iterations costs ~6× the heap.
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::RcFloor)));
        let small_off = compute(&prog(500)).expect("n=500 off");
        let big_off = compute(&prog(3000)).expect("n=3000 off");
        // RC-floor ON: the free-at-overwrite rule frees each dead buffer into the
        // size-classed free-list, where the next allocation reuses it — so the loop
        // is bounded, independent of iteration count.
        opt::set_for_tests(Some(OptSet::default_set().with(Opt::RcFloor)));
        let small_on = compute(&prog(500)).expect("n=500 on");
        let big_on = compute(&prog(3000)).expect("n=3000 on");
        opt::set_for_tests(None);
        // Each iteration inserts a fresh key then removes it: the dict ends empty —
        // identical observable output with the lever on or off (the parity contract).
        for r in [&small_off, &big_off, &small_on, &big_on] {
            assert_eq!(r.output, vec!["0".to_string()], "eviction loop must end empty");
        }
        // OFF: the pinned leak still holds (the lever is doing real work, not a no-op).
        assert!(
            big_off.heap_bytes > small_off.heap_bytes * 3,
            "without rc-floor the eviction garbage must still leak O(n): \
             n=500 heap={}, n=3000 heap={}",
            small_off.heap_bytes,
            big_off.heap_bytes
        );
        // ON: bounded — 6× the iterations stays within ~2× the heap (mirroring the
        // bounded-keyset pin), proving the floor reclaims the escaping garbage.
        assert!(
            big_on.heap_bytes < small_on.heap_bytes * 2,
            "rc-floor must bound the eviction loop, but heap scaled with iterations: \
             n=500 heap={}, n=3000 heap={}",
            small_on.heap_bytes,
            big_on.heap_bytes
        );
        // DoD counter (b): the `__rc_reused_bytes` stats counter PROVES the floor
        // fired — OFF it never reclaims (0), ON it recycles bytes that scale with
        // iteration count (every freed buffer is reused by the next allocation).
        assert_eq!(big_off.rc_reused_bytes, 0, "rc-floor off must not reclaim");
        assert!(
            big_on.rc_reused_bytes > big_off.rc_reused_bytes
                && big_on.rc_reused_bytes > small_on.rc_reused_bytes,
            "rc-floor must reclaim bytes that scale with iterations: \
             off={}, on(n=500)={}, on(n=3000)={}",
            big_off.rc_reused_bytes,
            small_on.rc_reused_bytes,
            big_on.rc_reused_bytes
        );
    }

    /// RFC-0016 RC-floor generalizes past dicts to the STRING primitive allocators:
    /// a confined string `var` transformed each iteration (`s = s.to_upper()` →
    /// `string.to_upper(s)`, a fresh same-length buffer via the now-`$rc_alloc`-routed
    /// `ascii_case`) frees its old buffer for reuse — so the loop is bounded with the
    /// floor on and leaks O(n) with it off, identical output either way.
    #[test]
    fn string_transform_bounded_by_rc_floor() {
        let prog = |n: i32| {
            format!(
                "fn main(console: Console):\n    var s = \"the quick brown fox jumps\"\n    var i = 0\n    while i < {n}:\n        s = s.to_upper()\n        s = s.to_lower()\n        i = i + 1\n    console.print(\"${{s.length()}}\")\n"
            )
        };
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::RcFloor)));
        let small_off = compute(&prog(500)).expect("off small");
        let big_off = compute(&prog(3000)).expect("off big");
        opt::set_for_tests(Some(OptSet::default_set().with(Opt::RcFloor)));
        let small_on = compute(&prog(500)).expect("on small");
        let big_on = compute(&prog(3000)).expect("on big");
        opt::set_for_tests(None);
        // to_upper/to_lower preserve length: the string stays 25 chars regardless.
        for r in [&small_off, &big_off, &small_on, &big_on] {
            assert_eq!(r.output, vec!["25".to_string()], "transform preserves length");
        }
        assert!(
            big_off.heap_bytes > small_off.heap_bytes * 3,
            "without rc-floor the string churn must leak O(n): n=500 {}, n=3000 {}",
            small_off.heap_bytes,
            big_off.heap_bytes
        );
        assert!(
            big_on.heap_bytes < small_on.heap_bytes * 2,
            "rc-floor must bound the string churn: n=500 {}, n=3000 {}",
            small_on.heap_bytes,
            big_on.heap_bytes
        );
        assert_eq!(big_off.rc_reused_bytes, 0, "rc-floor off must not reclaim");
        assert!(big_on.rc_reused_bytes > 0, "rc-floor must reclaim string buffers");
    }

    /// (RFC-0035) The Perceus dup/drop floor (steps 1-4) reclaims a container read-out + set_at
    /// churn loop to a BOUNDED live-object count. Each iteration reads an element into a binding,
    /// extracts its payload, and displaces the slot; under rc-floor the dup-at-read, the set_at
    /// displaced-drop, and the last-use drops release all of it, so `__witchy_live_cells` stays
    /// ~constant while the default leaks one Box (plus its String) per iteration. This is the DoD
    /// leak metric for the shared-value floor — a direct object count, sharper than heap bytes.
    #[test]
    fn read_out_churn_reclaimed_by_rc_floor() {
        let src = "import list\ntype Box:\n    Box(String)\nfn unwrap(b: Box) -> String:\n    match b:\n        Box(s) -> s\nfn main(console: Console):\n    var xs = [Box(\"a\"), Box(\"b\"), Box(\"c\"), Box(\"d\")]\n    var i = 0\n    while i < 2000:\n        let held = list.at(xs, 0)\n        let s = unwrap(held)\n        list.set_at(xs, 0, Box(\"z\"))\n        i = i + 1\n    console.print(unwrap(list.at(xs, 1)))\n";
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::RcFloor)));
        let off = compute(src).expect("off");
        opt::set_for_tests(Some(OptSet::default_set().with(Opt::RcFloor)));
        let on = compute(src).expect("on");
        opt::set_for_tests(None);
        assert_eq!(on.output, off.output, "rc-floor must not change output");
        assert_eq!(on.output, vec!["b".to_string()]);
        assert!(
            on.live_cells < 100,
            "rc-floor must reclaim the churn to bounded live cells, got {}",
            on.live_cells
        );
        assert!(
            off.live_cells > on.live_cells * 10,
            "default must leak far more live cells: off {} vs on {}",
            off.live_cells,
            on.live_cells
        );
    }

    /// (RFC-0059 DoD item 3, BLOCKED on the scalar-SoA executor — increment-2 step 2) The async
    /// channel executor must reclaim its per-message garbage to a BOUNDED live-cell count under
    /// rc-floor. Progress so far: RFC-0036 Design B (owned executor) bounded the O(n^2) array churn;
    /// RFC-0059 increment 1 (defunctionalized state-machine lowering) removed the `and_then` closure
    /// TOWER (one shallow continuation per resume, not O(depth)); RFC-0059 increment-2 step 1 made
    /// channels fixed-capacity RINGS (send = in-place `set_at`, recv = advance `head`, no `list.tail`
    /// rebuild). What REMAINS is per-message CLOSURE/Task/Step garbage from the CPS-over-closures
    /// executor INTERFACE — measured (2026-07-05, post-ring) at ~45–48 live cells PER MESSAGE, FLAT
    /// per message but LINEAR in N (N=200 → ~9.1k cells; N=64000 → ~3.07M; N=1M OOM-traps). The ring
    /// does NOT move this: the round-robin schedule keeps buffer occupancy at ~1, so the buffer churn
    /// was already reclaimed — the leak is the segment closures `fn(x): __seg(carried, x)`, their
    /// `Task`/`Step` wrappers, and the erased `__Msg`, whose heap children shell-only drop cannot
    /// free. Closing it to `< 500` requires increment-2 step 2 (scalar SoA frames): reify each
    /// segment's carried columns as scalar `Int` columns indexed by task id + defunctionalize the
    /// continuation to a `(seg-id, task-id)` dispatch, so a resume allocates NOTHING (the reference
    /// spike proved 13 cells FLAT to N=1M, 10 ns/msg). The alternative — recursive `$rdrop` for the
    /// closure shapes — is the highest use-after-free-risk path and stays blocked on the per-capture
    /// move/borrow oracle. Un-ignore when the executor reclaims. See rfcs/0059.
    #[test]
    #[ignore = "chan_throughput closure garbage not yet reclaimed — needs the scalar-SoA executor (RFC-0059 increment-2 step 2). Increment 1 (state-machine lowering) + increment-2 step 1 (ring channels) landed; ~45–48 live cells/message remain (measures live_cells≈9.1k at N=200, cap8, via --run-ignored — the CPS closure/Task/Step interface churn, unaffected by the ring). See rfcs/0059 note 2026-07-05"]
    fn chan_throughput_bounded_by_rc_floor() {
        let src = "from chan import Receiver, Sender\nasync fn producer(tx: Sender(Int), n: Int) -> Nil:\n    for i in 0..n:\n        chan.send(tx, i).await\nasync fn main(console: Console):\n    let (tx, rx) = chan.channel(8).await\n    chan.spawn(producer(tx, 200)).await\n    for await v in rx:\n        chan.done(v)\n    console.print(\"200\")\n";
        opt::set_for_tests(Some(OptSet::default_set().with(Opt::RcFloor)));
        let on = compute(src).expect("compile+run executor");
        opt::set_for_tests(None);
        assert_eq!(on.output, vec!["200".to_string()]);
        assert!(
            on.live_cells < 500,
            "the executor must reclaim its per-message garbage to bounded live cells, got {}",
            on.live_cells
        );
    }

    /// (RFC-0059 increment-2 step 2 — the FLAT TARGET, proven falsifiably in-tree.)
    /// The scalar-SoA + ring representation the async transform must PRODUCE: a
    /// producer/consumer over a bounded ring, hand-written so every per-task datum is
    /// a scalar `Int` column (`f0 = [np, sum]`, `f1 = [i, seen]`, `status`) mutated by
    /// `list.set_at`, the channel a fixed-capacity ring (`ring`/`head`/`tail`/`count`).
    /// No closures, no `Task`/`Step`, no per-message allocation — so under rc-floor the
    /// live-cell count is FLAT (measured 13, IDENTICAL at N=200 and N=20000), exactly
    /// the shape `chan_throughput_bounded_by_rc_floor` (above, still `#[ignore]`d) needs
    /// the executor to reach. This is the executable, re-runnable form of the RFC-0059
    /// 2026-07-05 spike numbers (prose numbers rot; this asserts the property) and it
    /// guards against a codegen regression silently breaking the scalar-flat property
    /// step 2 depends on. Kernel-timed separately at ~11 ns/message flat to N=1M (under
    /// the ≤300 ns DoD, ≤100 ns stretch); this test pins the flat-memory half.
    #[test]
    fn chan_throughput_scalar_soa_reference_is_flat() {
        // The all-scalar producer/consumer kernel, parametric in N (ring cap 64).
        let soa_src = |n: i64| {
            format!(
                "import list\n\nfn wrap(x: Int, m: Int) -> Int:\n    if x >= m: x - m else: x\n\nfn run(np: Int, cap: Int) -> Int:\n    var ring = list.range_between(0, cap)\n    var head = 0\n    var tail = 0\n    var count = 0\n    var status = [0, 0]\n    var f0 = [np, 0]\n    var f1 = [0, 0]\n    var go = true\n    while go:\n        var prog = false\n        if list.at(status, 0) != 3:\n            if count < cap:\n                let i = list.at(f1, 0)\n                if i < list.at(f0, 0):\n                    list.set_at(ring, tail, i)\n                    tail = wrap(tail + 1, cap)\n                    count = count + 1\n                    list.set_at(f1, 0, i + 1)\n                    prog = true\n                else:\n                    list.set_at(status, 0, 3)\n                    prog = true\n        if list.at(status, 1) != 3:\n            if count > 0:\n                let v = list.at(ring, head)\n                head = wrap(head + 1, cap)\n                count = count - 1\n                list.set_at(f0, 1, list.at(f0, 1) + v)\n                let seen = list.at(f1, 1) + 1\n                list.set_at(f1, 1, seen)\n                if seen >= np:\n                    list.set_at(status, 1, 3)\n                prog = true\n        if list.at(status, 0) == 3 && list.at(status, 1) == 3:\n            go = false\n        else if prog:\n            go = true\n        else:\n            go = false\n    list.at(f0, 1)\n\nfn main(console: Console):\n    console.print(\"${{run({n}, 64)}}\")\n",
                n = n,
            )
        };
        opt::set_for_tests(Some(OptSet::default_set().with(Opt::RcFloor)));
        // sum 0..N-1: N=200 -> 19900; N=20000 -> 199990000.
        let small = compute(&soa_src(200)).expect("compile+run small");
        let big = compute(&soa_src(20000)).expect("compile+run big");
        opt::set_for_tests(None);
        assert_eq!(small.output, vec!["19900".to_string()], "small sum wrong");
        assert_eq!(big.output, vec!["199990000".to_string()], "big sum wrong");
        // FLAT: a small constant, and IDENTICAL at N=200 and N=20000 (independent of N).
        assert!(
            small.live_cells < 100,
            "scalar-SoA reference must be flat (bounded live cells), got {} @ N=200",
            small.live_cells
        );
        assert_eq!(
            big.live_cells, small.live_cells,
            "scalar-SoA reference must be FLAT: live_cells must not grow with N ({} @ 20000 vs {} @ 200)",
            big.live_cells, small.live_cells
        );
    }

    /// RFC-0027 packed DoD counter (b): a confined list literal of fixed-scalar
    /// records read only via `at(_).field`/`length` is stored as ONE flat inline
    /// buffer with `unbox` on, instead of N boxed records + an N-pointer array with
    /// it off — so the heap drops by the pointer array plus every per-record header.
    /// Output is identical (parity). `unbox` is opt-in, so this compares `all`
    /// (unbox on) against `all` minus `unbox`.
    #[test]
    fn packed_record_list_uses_one_flat_buffer() {
        // 10 two-field records: boxed = 10*(4+16) records + (4+10*8) list; packed =
        // one (4 + 10*2*8) buffer — a ~120-byte drop (pointer array + tag headers).
        let src = "import list\n\ntype P:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let ps = [P(0, 1), P(2, 3), P(4, 5), P(6, 7), P(8, 9), P(10, 11), P(12, 13), P(14, 15), P(16, 17), P(18, 19)]\n    var t = 0\n    var i = 0\n    while i < list.length(ps):\n        t = t + list.at(ps, i).x + list.at(ps, i).y\n        i = i + 1\n    console.print(\"${t}\")\n";
        opt::set_for_tests(Some(OptSet::all()));
        let on = compute(src).expect("unbox on");
        opt::set_for_tests(Some(OptSet::all().without(Opt::Unbox)));
        let off = compute(src).expect("unbox off");
        opt::set_for_tests(None);

        // Sum 0..19 = 190, layout-independent.
        assert_eq!(on.output, off.output, "unbox must not change output");
        assert_eq!(on.output, vec!["190".to_string()]);
        // The flat buffer drops the pointer array + per-record headers.
        assert!(
            off.heap_bytes >= on.heap_bytes + 100,
            "packed must use less heap than the boxed layout: on={} off={}",
            on.heap_bytes,
            off.heap_bytes
        );
    }

    /// RFC-0027 declared `packed` DoD: a `type P packed:` whose confined list is read
    /// only via `list.length`/`list.at(_).field` is stored as ONE flat inline buffer
    /// under the `unbox` lever (the same layout the inference uses, now GUARANTEED by
    /// the declaration), dropping the pointer array + per-record headers — identical
    /// output to the boxed layout (the representation parity contract).
    #[test]
    fn declared_packed_list_packs_flat() {
        let src = "import list\n\ntype P packed:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let ps = [P(0, 1), P(2, 3), P(4, 5), P(6, 7), P(8, 9), P(10, 11), P(12, 13), P(14, 15), P(16, 17), P(18, 19)]\n    var t = 0\n    var i = 0\n    while i < list.length(ps):\n        t = t + list.at(ps, i).x + list.at(ps, i).y\n        i = i + 1\n    console.print(\"${t}\")\n";
        opt::set_for_tests(Some(OptSet::all()));
        let on = compute(src).expect("unbox on");
        opt::set_for_tests(Some(OptSet::all().without(Opt::Unbox)));
        let off = compute(src).expect("unbox off");
        opt::set_for_tests(None);
        // Sum 0..19 = 190, representation-independent (the parity contract).
        assert_eq!(on.output, off.output, "packed layout must not change output");
        assert_eq!(on.output, vec!["190".to_string()]);
        assert!(
            off.heap_bytes >= on.heap_bytes + 100,
            "declared-packed must use less heap than boxed: on={} off={}",
            on.heap_bytes,
            off.heap_bytes
        );
    }

    /// RFC-0027 declared `packed` soundness: a packed list used as a WHOLE value
    /// in-body (not the confined `list.length`/`list.at(_).field` shape) — here
    /// aliased to another binding — is a clean codegen COMPILE ERROR, never a silent
    /// miscompile against the flat layout. The complement of the typeck boundary
    /// reject: this is the in-function `reject_reason` path.
    #[test]
    fn declared_packed_whole_value_use_is_rejected() {
        let src = "import list\n\ntype P packed:\n    x: Int\n\nfn main(console: Console):\n    let xs = [P(1), P(2)]\n    let ys = xs\n    console.print(\"${list.length(ys)}\")\n";
        opt::set_for_tests(Some(OptSet::default_set()));
        let r = compute(src);
        opt::set_for_tests(None);
        assert!(r.is_err(), "a declared-packed list used as a whole value must be rejected, got {r:?}");
    }

    /// RFC-0028 `for var`: a mutation of the loop element is written back into the
    /// list, identically on the interpreter and on the WASM backend (default and
    /// forced-copy) — the parity contract for the new ergonomic form.
    #[test]
    fn for_var_writes_elements_back_on_both_backends() {
        let src = "fn main(console: Console):\n    var xs = [1, 2, 3, 4]\n    for var x in xs:\n        x = x * 10\n    console.print(\"${xs}\")\n";
        let oracle = interp(src);
        assert_eq!(oracle, vec!["[10, 20, 30, 40]".to_string()]);
        opt::set_for_tests(Some(OptSet::default_set()));
        let wasm = compute(src).expect("wasm").output;
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::InPlace)));
        let wasm_copy = compute(src).expect("wasm -inplace").output;
        opt::set_for_tests(None);
        assert_eq!(wasm, oracle, "for var: wasm must match the interpreter");
        assert_eq!(wasm_copy, oracle, "for var: forced-copy must match too");
    }

    /// `for var` mutating a record field of each element, parity-checked.
    #[test]
    fn for_var_mutates_record_fields() {
        let src = "type P:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    var ps = [P(1, 2), P(3, 4)]\n    for var p in ps:\n        p.x = p.x + 100\n    console.print(\"${ps}\")\n";
        let oracle = interp(src);
        assert!(oracle[0].contains("101") && oracle[0].contains("103"), "{oracle:?}");
        opt::set_for_tests(Some(OptSet::default_set()));
        let wasm = compute(src).expect("wasm").output;
        opt::set_for_tests(None);
        assert_eq!(wasm, oracle, "for var record mutation: wasm must match the interpreter");
    }

    /// RFC-0028 `for var` DoD counter (b): the element write-back is in-place, so
    /// rewriting every element stays O(n) heap; with `-inplace` the same loop
    /// re-allocates the whole list per element (O(n^2)). Output is identical.
    #[test]
    fn for_var_writeback_is_in_place() {
        let src = "fn main(console: Console):\n    var xs = []\n    var i = 0\n    while i < 300:\n        list.push(xs, i)\n        i = i + 1\n    for var x in xs:\n        x = x + 1\n    console.print(\"${xs.at(0)}\")\n    console.print(\"${xs.at(299)}\")\n";
        opt::set_for_tests(Some(OptSet::default_set()));
        let on = compute(src).expect("inplace on");
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::InPlace)));
        let off = compute(src).expect("inplace off");
        opt::set_for_tests(None);
        assert_eq!(on.output, off.output, "for var output must not depend on the optimization");
        assert_eq!(on.output, vec!["1".to_string(), "300".to_string()]);
        assert!(
            off.heap_bytes > on.heap_bytes * 2,
            "for var write-back must use the in-place path: on={} off={}",
            on.heap_bytes,
            off.heap_bytes
        );
    }

    /// RFC-0028/0043 `nodes.push(x)`: a statement-position mutator call (declared
    /// `var` receiver) writes back to the place, identically on both backends.
    /// A non-mutator method (`length`) discarded in statement form is now a
    /// compile error (RFC-0043), so it must be an explicit `let _ =` discard for
    /// the program to type-check.
    #[test]
    fn mutating_method_statement_writes_back_on_both_backends() {
        let src = "fn main(console: Console):\n    var xs = []\n    xs.push(1)\n    xs.push(2)\n    xs.push(3)\n    var d = dict.new()\n    d.insert(\"a\", 7)\n    var ys = [9, 9, 9]\n    let _ = ys.length()\n    console.print(\"${xs}\")\n    console.print(\"${dict.get_or(d, \"a\", 0)}\")\n";
        let oracle = interp(src);
        assert_eq!(oracle, vec!["[1, 2, 3]".to_string(), "7".to_string()]);
        opt::set_for_tests(Some(OptSet::default_set()));
        let wasm = compute(src).expect("wasm").output;
        opt::set_for_tests(None);
        assert_eq!(wasm, oracle, "nodes.push(x): wasm must match the interpreter");
    }

    /// `nodes.push(x)` DoD counter (b): the statement uses the in-place path, so a
    /// push loop stays O(n) heap; with `-inplace` it is O(n^2). Same output.
    #[test]
    fn mutating_method_statement_is_in_place() {
        let src = "fn main(console: Console):\n    var xs = []\n    var i = 0\n    while i < 300:\n        xs.push(i)\n        i = i + 1\n    console.print(\"${list.length(xs)}\")\n";
        opt::set_for_tests(Some(OptSet::default_set()));
        let on = compute(src).expect("on");
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::InPlace)));
        let off = compute(src).expect("off");
        opt::set_for_tests(None);
        assert_eq!(on.output, off.output);
        assert_eq!(on.output, vec!["300".to_string()]);
        assert!(
            off.heap_bytes > on.heap_bytes * 2,
            "push statement must use the in-place path: on={} off={}",
            on.heap_bytes,
            off.heap_bytes
        );
    }

    /// Regression (the `next_row`/pascal failure): a pure method call in tail
    /// position is the block's value, while an earlier `var` call is a statement.
    #[test]
    fn tail_position_method_call_is_not_rewritten() {
        let src = "fn grow(row: List(Int)) -> List(Int):\n    var out = [0]\n    out.push(row.at(0))\n    out.concat([99])\n\nfn main(console: Console):\n    console.print(\"${grow([7])}\")\n";
        // `push` writes back in statement position; pure `concat` supplies the tail value.
        let oracle = interp(src);
        assert_eq!(oracle, vec!["[0, 7, 99]".to_string()]);
        opt::set_for_tests(Some(OptSet::default_set()));
        let wasm = compute(src).expect("wasm").output;
        opt::set_for_tests(None);
        assert_eq!(wasm, oracle, "tail-position method call must agree on both backends");
    }

    /// RFC-0027 escape-driven SROA: a frame-confined record built each iteration
    /// and read only via field access is scalar-replaced into locals — zero heap
    /// growth with `sroa` on, O(n) with `-sroa`. Region is turned off so the
    /// accumulated record allocations are visible (not watermark-reclaimed). Output
    /// is identical (the differential sweep also walks `-sroa`).
    #[test]
    fn sroa_eliminates_confined_aggregate_allocation() {
        let src = "type P:\n    x: Int\n    y: Int\nfn main(console: Console):\n    var total = 0\n    var i = 0\n    while i < 300:\n        let p = P(i, i + 1)\n        total = total + p.x + p.y\n        i = i + 1\n    console.print(\"${total}\")\n";
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::Region)));
        let on = compute(src).expect("sroa on");
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::Region).without(Opt::Sroa)));
        let off = compute(src).expect("sroa off");
        opt::set_for_tests(None);
        assert_eq!(on.output, off.output, "SROA must not change output");
        assert_eq!(on.output, vec!["90000".to_string()]);
        assert!(
            off.heap_bytes > on.heap_bytes * 4,
            "SROA must remove the per-iteration record alloc: on={} off={}",
            on.heap_bytes,
            off.heap_bytes
        );
    }

    /// SROA also covers a MUTABLE record field-written in a loop (`var p = ...;
    /// p.x = ...`): the field updates write the slot locals in place, so still no
    /// heap object — O(1) vs O(n). Identical output.
    #[test]
    fn sroa_handles_mutable_field_written_record() {
        let src = "type P:\n    x: Int\n    y: Int\nfn main(console: Console):\n    var total = 0\n    var i = 0\n    while i < 300:\n        var p = P(i, 0)\n        p.x = p.x + 1\n        p.y = p.x * 2\n        total = total + p.x + p.y\n        i = i + 1\n    console.print(\"${total}\")\n";
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::Region)));
        let on = compute(src).expect("sroa on");
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::Region).without(Opt::Sroa)));
        let off = compute(src).expect("sroa off");
        opt::set_for_tests(None);
        assert_eq!(on.output, off.output, "mutable-record SROA must not change output");
        assert_eq!(on.output, vec!["135450".to_string()]);
        assert!(
            off.heap_bytes > on.heap_bytes * 4,
            "mutable-record SROA must remove the per-iteration alloc: on={} off={}",
            on.heap_bytes,
            off.heap_bytes
        );
    }

    /// RFC-0030 `fold`: constant folding + propagation elides constant string
    /// concatenations (`g + "World"` where `g` is a literal `let`) — the allocation
    /// the WASM mid-end can't see. heap drops sharply with `fold` on; output is
    /// identical (folding is semantics-preserving).
    #[test]
    fn fold_elides_constant_concatenations() {
        let src = "fn main(console: Console):\n    let g = \"Hi, \"\n    var total = 0\n    var i = 0\n    while i < 300:\n        let s = g + \"World\" + g + \"There\"\n        total = total + string.length(s)\n        i = i + 1\n    console.print(\"${total}\")\n";
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::Region)));
        let on = compute(src).expect("fold on");
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::Region).without(Opt::Fold)));
        let off = compute(src).expect("fold off");
        opt::set_for_tests(None);
        assert_eq!(on.output, off.output, "fold must not change output");
        assert_eq!(on.output, vec!["5400".to_string()]);
        assert!(
            off.heap_bytes > on.heap_bytes * 4,
            "fold must elide constant-concat allocations: on={} off={}",
            on.heap_bytes,
            off.heap_bytes
        );
    }

    /// The goal's NEVER-OOM property: a long loop with transient list/string
    /// scratch AND a helper that allocates internally but returns a scalar runs in
    /// CONSTANT heap — loop-watermark reclaim + SROA + in-place keep it O(1)
    /// regardless of iteration count. 5000 iterations stay under a tiny budget.
    #[test]
    fn never_oom_long_loop_stays_bounded() {
        let src = "fn cost(n: Int) -> Int:\n    let tmp = [n, n + 1, n + 2]\n    list.length(tmp) + string.length(\"${n}\")\nfn main(console: Console):\n    var total = 0\n    var i = 0\n    while i < 5000:\n        let scratch = [i, i + 1]\n        total = total + cost(i) + list.length(scratch)\n        i = i + 1\n    console.print(\"${total}\")\n";
        opt::set_for_tests(Some(OptSet::default_set()));
        let s = compute(src).expect("compute");
        opt::set_for_tests(None);
        assert_eq!(s.output, vec!["43890".to_string()]);
        assert!(
            s.heap_bytes < 4096,
            "never-OOM: 5000 iterations must stay in bounded heap, got {}",
            s.heap_bytes
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
