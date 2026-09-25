//! Layout invariants driven through `witchy_lower::canonical_layout_self_checks`
//! (the checks need private layout internals; the crate has no lib-test build).

use witchy_lower::canonical_layout_self_checks as checks;

#[test]
fn noncompletion_still_requires_checked_memory_bearing_descendants() {
    checks::noncompletion_still_requires_checked_memory_bearing_descendants();
}

#[test]
fn rooted_slice_layout_requires_explicit_view_slice_request() {
    checks::rooted_slice_layout_requires_explicit_view_slice_request();
}

#[test]
fn direct_and_nested_capabilities_never_acquire_linear_slots() {
    checks::direct_and_nested_capabilities_never_acquire_linear_slots();
}
