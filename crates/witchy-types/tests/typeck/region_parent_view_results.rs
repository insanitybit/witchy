//! RFC-0167 §8 / row L (SEC-096): a `region:` result aggregate may carry
//! views whose roots are parent-owned. A record, list, `Option` or tuple
//! leaves the region when every view it holds is proven parent-rooted, per
//! field: formed as `&place` over a root declared outside the region, or read
//! from a binding that is itself parent-rooted. A view of a region-born root,
//! or one whose provenance is unknown (a call result), is still refused.
//!
//! Evidence: an independent expected result and admitted unoptimized Wasm
//! with a zero-leak heap audit. SafeWIR admission is required; the canonical
//! interpreter is not consulted.

use super::source::checked;

fn assert_refused(source: &str, needle: &str) {
    let error = checked(source).err()
        .expect("the program is refused at checking");
    assert!(error.contains(needle), "refusal names the rule: {error}");
}

#[test]
fn a_view_from_a_call_result_in_a_region_is_refused() {
    assert_refused(
        "type Holder:\n    v: &Int\n\nfn pick(x: &Int) -> &Int:\n    x\n\nfn main() -> Int:\n    let parent = 7\n    let h = region -> Holder:\n        Holder(v: pick(&parent))\n    *h.v\n",
        "per-field parent-root provenance",
    );
}
