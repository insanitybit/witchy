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
}
