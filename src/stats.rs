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
        rc_reused_bytes: vm.rc_reused_bytes().unwrap_or(0),
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
            // confined slice VIEW: a read-only window over an unmutated param,
            // read only via at/length — toggling `views` must not change output.
            "import list\n\nfn win(xs: List(Int), lo: Int, hi: Int) -> Int:\n    let w = list.slice(xs, lo, hi)\n    var t = 0\n    var j = 0\n    while j < list.length(w):\n        t = t + list.at(w, j)\n        j = j + 1\n    t\n\nfn main(console: Console):\n    let xs = [10, 20, 30, 40, 50, 60]\n    print(console, __render(win(xs, 1, 4)))\n    print(console, __render(win(xs, 4, 100)))\n    print(console, __render(win(xs, 2, 2)))\n",
            // PACKED confined record-list: a list literal of fixed-scalar records
            // read only via at(_).field / length — toggling `unbox` (on under
            // `all`) must not change output.
            "import list\n\ntype P:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let ps = [P(1, 2), P(3, 4), P(5, 6)]\n    var t = 0\n    var i = 0\n    while i < list.length(ps):\n        t = t + list.at(ps, i).x * 10 + list.at(ps, i).y\n        i = i + 1\n    print(console, __render(t))\n    print(console, __render(list.length(ps)))\n",
            // RC-floor REUSE: a confined var reassigned to same-length list literals,
            // read only via at/length — toggling `rc-elide` (in-place overwrite vs
            // fresh alloc) must not change output.
            "import list\n\nfn main(console: Console):\n    var v = [0, 0, 0]\n    var i = 0\n    while i < 5:\n        v = [i, i * 2, i * 3]\n        i = i + 1\n    print(console, __render(list.at(v, 0) + list.at(v, 1) + list.at(v, 2)))\n",
            // RC-floor REUSE (record): a confined var reassigned to the same ctor,
            // read only via fields — `rc-elide` overwrites the field slots in place.
            "type Point:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    var p = Point(0, 0)\n    var i = 0\n    while i < 5:\n        p = Point(i, i * 2)\n        i = i + 1\n    print(console, __render(p.x + p.y))\n",
            // CACHE EVICTION: insert then remove distinct dict keys (the per-object
            // RC floor's target garbage). Output must stay invariant under every
            // `WITCHY_OPT` setting and match the interpreter — the parity guard for
            // the residual the floor will eventually bound (see
            // `cache_eviction_leaks_without_rc_floor`).
            "import dict\n\nfn main(console: Console):\n    var d = dict.new()\n    var i = 0\n    while i < 40:\n        d = dict.insert(d, i, i * 2)\n        d = dict.remove(d, i)\n        i = i + 1\n    print(console, __render(dict.length(d)))\n    d = dict.insert(d, 7, 70)\n    print(console, __render(dict.get_or(d, 7, 0)))\n",
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
        let src = "import list\n\nfn win(xs: List(Int)) -> Int:\n    let w = list.slice(xs, 0, 400)\n    var t = 0\n    var j = 0\n    while j < list.length(w):\n        t = t + list.at(w, j)\n        j = j + 1\n    t\n\nfn main(console: Console):\n    var xs = []\n    var i = 0\n    while i < 400:\n        xs = list.push(xs, i)\n        i = i + 1\n    print(console, __render(win(xs)))\n";
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
                "fn main(console: Console):\n    var latest = [0]\n    var i = 0\n    while i < {n}:\n        latest = [i, i + 1, i + 2]\n        i = i + 1\n    print(console, __render(list.length(latest)))\n"
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
                "fn main(console: Console):\n    var latest = [0, 0, 0]\n    var i = 0\n    while i < {n}:\n        latest = [i, i + 1, i + 2]\n        i = i + 1\n    print(console, __render(list.at(latest, 0) + list.at(latest, 2) + list.length(latest)))\n"
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
                "type Point:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    var p = Point(0, 0)\n    var i = 0\n    while i < {n}:\n        p = Point(i, i * 2)\n        i = i + 1\n    print(console, __render(p.x + p.y))\n"
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
                "import dict\n\nfn main(console: Console):\n    var d = dict.new()\n    var i = 0\n    while i < {n}:\n        d = dict.insert(d, i % 8, i)\n        i = i + 1\n    print(console, __render(dict.length(d)))\n"
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
                "import dict\n\nfn main(console: Console):\n    var d = dict.new()\n    var i = 0\n    while i < {n}:\n        d = dict.insert(d, i, i)\n        d = dict.remove(d, i)\n        i = i + 1\n    print(console, __render(dict.length(d)))\n"
            )
        };
        // RC-floor OFF (the opt-in lever absent): the eviction garbage leaks O(n).
        // Every `dict.remove` allocates a fresh buffer and the old, dead, uniquely
        // owned one is never reclaimed — so 6× the iterations costs ~6× the heap.
        opt::set_for_tests(Some(OptSet::default_set()));
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
                "fn main(console: Console):\n    var s = \"the quick brown fox jumps\"\n    var i = 0\n    while i < {n}:\n        s = s.to_upper()\n        s = s.to_lower()\n        i = i + 1\n    print(console, __render(s.length()))\n"
            )
        };
        opt::set_for_tests(Some(OptSet::default_set()));
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
        let src = "import list\n\ntype P:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let ps = [P(0, 1), P(2, 3), P(4, 5), P(6, 7), P(8, 9), P(10, 11), P(12, 13), P(14, 15), P(16, 17), P(18, 19)]\n    var t = 0\n    var i = 0\n    while i < list.length(ps):\n        t = t + list.at(ps, i).x + list.at(ps, i).y\n        i = i + 1\n    print(console, __render(t))\n";
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

    /// The goal's NEVER-OOM property: a long loop with transient list/string
    /// scratch AND a helper that allocates internally but returns a scalar runs in
    /// CONSTANT heap — loop-watermark reclaim + SROA + in-place keep it O(1)
    /// regardless of iteration count. 5000 iterations stay under a tiny budget.
    #[test]
    fn never_oom_long_loop_stays_bounded() {
        let src = "fn cost(n: Int) -> Int:\n    let tmp = [n, n + 1, n + 2]\n    list.length(tmp) + string.length(\"${n}\")\nfn main(console: Console):\n    var total = 0\n    var i = 0\n    while i < 5000:\n        let scratch = [i, i + 1]\n        total = total + cost(i) + list.length(scratch)\n        i = i + 1\n    print(console, __render(total))\n";
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
        let bad = "fn main(console: Console):\n    var xs = [1, 2, 3]\n    for var x in xs:\n        if x == 2:\n            continue\n        x = x * 10\n    print(console, __render(xs))\n";
        assert!(crate::resolve_std_only(bad).is_err(), "a loop-belonging continue must be rejected");
        let ok = "fn main(console: Console):\n    var xs = [1, 2, 3]\n    for var x in xs:\n        for y in [0, 1]:\n            if y == 9:\n                continue\n        x = x * 10\n    print(console, __render(xs))\n";
        assert!(crate::resolve_std_only(ok).is_ok(), "a nested-loop continue must be allowed");
    }
}
