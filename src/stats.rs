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
}

/// Compile `src` (resolved against the bundled std) and run it under the active
/// `WITCHY_OPT` setting, returning its deterministic counters. The program must
/// need nothing beyond `Console` (the stats corpus is pure compute).
pub fn compute(src: &str) -> Result<Stats, String> {
    let linked = crate::resolve_std_only(src)?;
    typeck::check(&linked).map_err(|e| format!("{e:?}"))?;
    let bytes = codegen::compile_module_binary(&linked)
        .map_err(|e| e.message)?
        .ok_or_else(|| "program does not fully lower to the compiled backend".to_string())?;
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opt::{self, Opt, OptSet};

    // An accumulation loop: in-place push keeps it O(n); forced copy re-owns at
    // every iteration and balloons the heap to O(n^2).
    const ACC: &str = "fn main(console: Console):\n    var xs = []\n    var i = 0\n    while i < 400:\n        xs = list.push(xs, i)\n        i = i + 1\n    print(console, __render(list.length(xs)))\n";

    // A soak loop: per-iteration scratch that never escapes. The `region`
    // (loop-watermark) reclaim resets the arena each iteration, so heap stays
    // CONSTANT no matter how many iterations; with it off the same program leaks.
    const SOAK: &str = "fn main(console: Console):\n    var sum = 0\n    var i = 0\n    while i < 5000:\n        let tmp = [i, i + 1, i + 2, i + 3]\n        sum = sum + list.length(tmp)\n        i = i + 1\n    print(console, __render(sum))\n";

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
            "fn main(console: Console):\n    var xs = []\n    var s = \"\"\n    var d = dict.new()\n    var i = 0\n    while i < 300:\n        let scratch = [i, i + 1]\n        xs.push(i + list.length(scratch) - 2)\n        s = s + __render(i % 10)\n        d = dict.update(d, i % 7, 0, fn(n: Int): n + 1)\n        i = i + 1\n    let folded = 2 * 3 + 4\n    print(console, __render(list.length(xs)))\n    print(console, __render(string.length(s)))\n    print(console, __render(dict.get_or(d, 3, 0)))\n    print(console, __render(folded))\n",
            // `for var` write-back over record elements.
            "type P:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    var ps = [P(1, 2), P(3, 4)]\n    for var p in ps:\n        p.x = p.x + 100\n    print(console, \"${ps}\")\n",
        ];
        for src in corpus {
            // The interpreter oracle (the fixed semantics; it has no WITCHY_OPT).
            let linked = crate::resolve_std_only(src).expect("link");
            typeck::check(&linked).expect("typeck");
            let oracle =
                crate::interpreter::run_module(linked, ".", Vec::new()).expect("interp run");

            opt::set_for_tests(Some(OptSet::all()));
            let base = compute(src).expect("compute all").output;
            opt::set_for_tests(None);
            assert_eq!(base, oracle, "wasm (all) must match the interpreter oracle");

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
                assert_eq!(out, base, "WITCHY_OPT={label} changed observable output");
            }
        }
    }

    fn interp(src: &str) -> Vec<String> {
        let linked = crate::resolve_std_only(src).expect("link");
        typeck::check(&linked).expect("typeck");
        crate::interpreter::run_module(linked, ".", Vec::new()).expect("interp run")
    }

    /// RFC-0028 `for var`: a mutation of the loop element is written back into the
    /// list, identically on the interpreter and on the WASM backend (default and
    /// forced-copy) — the parity contract for the new ergonomic form.
    #[test]
    fn for_var_writes_elements_back_on_both_backends() {
        let src = "fn main(console: Console):\n    var xs = [1, 2, 3, 4]\n    for var x in xs:\n        x = x * 10\n    print(console, __render(xs))\n";
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
        let src = "type P:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    var ps = [P(1, 2), P(3, 4)]\n    for var p in ps:\n        p.x = p.x + 100\n    print(console, __render(ps))\n";
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
        let src = "fn main(console: Console):\n    var xs = []\n    var i = 0\n    while i < 300:\n        xs = list.push(xs, i)\n        i = i + 1\n    for var x in xs:\n        x = x + 1\n    print(console, __render(xs.at(0)))\n    print(console, __render(xs.at(299)))\n";
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

    /// RFC-0028 `nodes.push(x)`: a statement-position mutating-method call writes
    /// back to the place, identically on both backends. A non-self-returning
    /// method (`length`) stays a discard, so the program still type-checks.
    #[test]
    fn mutating_method_statement_writes_back_on_both_backends() {
        let src = "fn main(console: Console):\n    var xs = []\n    xs.push(1)\n    xs.push(2)\n    xs.push(3)\n    var d = dict.new()\n    d.insert(\"a\", 7)\n    var ys = [9, 9, 9]\n    ys.length()\n    print(console, \"${xs}\")\n    print(console, __render(dict.get_or(d, \"a\", 0)))\n";
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
        let src = "fn main(console: Console):\n    var xs = []\n    var i = 0\n    while i < 300:\n        xs.push(i)\n        i = i + 1\n    print(console, __render(list.length(xs)))\n";
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

    /// Regression (the `next_row`/pascal failure): a self-returning method call in
    /// TAIL position is the block's value — the function's return — so it must NOT
    /// be rewritten to a (Nil-valued) write-back. A non-tail one still is.
    #[test]
    fn tail_position_method_call_is_not_rewritten() {
        let src = "fn grow(row: List(Int)) -> List(Int):\n    var out = [0]\n    out.push(row.at(0))\n    out.push(99)\n\nfn main(console: Console):\n    print(console, \"${grow([7])}\")\n";
        // out.push(row.at(0)) is non-tail -> rewritten (out becomes [0, 7]);
        // out.push(99) is the TAIL -> the returned value [0, 7, 99], not a discard.
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
        let src = "type P:\n    x: Int\n    y: Int\nfn main(console: Console):\n    var total = 0\n    var i = 0\n    while i < 300:\n        let p = P(i, i + 1)\n        total = total + p.x + p.y\n        i = i + 1\n    print(console, __render(total))\n";
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
        let src = "type P:\n    x: Int\n    y: Int\nfn main(console: Console):\n    var total = 0\n    var i = 0\n    while i < 300:\n        var p = P(i, 0)\n        p.x = p.x + 1\n        p.y = p.x * 2\n        total = total + p.x + p.y\n        i = i + 1\n    print(console, __render(total))\n";
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
        let src = "fn main(console: Console):\n    let g = \"Hi, \"\n    var total = 0\n    var i = 0\n    while i < 300:\n        let s = g + \"World\" + g + \"There\"\n        total = total + string.length(s)\n        i = i + 1\n    print(console, __render(total))\n";
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

    /// `for var` v1 rejects an early exit that belongs to the loop (it would skip
    /// the write-back), but allows a `continue` that belongs to a NESTED loop.
    #[test]
    fn for_var_rejects_loop_belonging_early_exit() {
        let bad = "fn main(console: Console):\n    var xs = [1, 2, 3]\n    for var x in xs:\n        if x == 2:\n            continue\n        x = x * 10\n    print(console, __render(xs))\n";
        assert!(crate::resolve_std_only(bad).is_err(), "a loop-belonging continue must be rejected");
        let ok = "fn main(console: Console):\n    var xs = [1, 2, 3]\n    for var x in xs:\n        for y in [0, 1]:\n            if y == 9:\n                continue\n        x = x * 10\n    print(console, __render(xs))\n";
        assert!(crate::resolve_std_only(ok).is_ok(), "a nested-loop continue must be allowed");
    }
}
