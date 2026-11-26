//! Geometry of the mapping containing a task stack and its two guards.

pub(super) const MAX_STACK: usize = 1024 * 1024;
const MIN_STACK: usize = 64 * 1024;
pub(super) const GUARD: usize = 256 * 1024;

/// The caller validates the requested size before querying the OS page size.
#[inline(always)]
pub(super) fn stack_layout(bytes: usize, page: usize) -> Option<(usize, usize)> {
    if page == 0 || GUARD % page != 0 {
        return None;
    }
    let stack = bytes.max(MIN_STACK).div_ceil(page) * page;
    Some((stack, stack + 2 * GUARD))
}

#[cfg(kani)]
#[kani::proof]
fn accepted_task_stack_fits_between_page_aligned_guards() {
    let bytes: usize = kani::any();
    let page: usize = kani::any();
    kani::assume((512..=MAX_STACK).contains(&bytes) && bytes % 16 == 0);
    // AArch64 mappings have a page granule of 4, 16, or 64 KiB.
    kani::assume(matches!(page, 4096 | 16384 | 65536));
    let (stack, mapping) = stack_layout(bytes, page).unwrap();
    let top = GUARD + stack;
    let requested = bytes.max(MIN_STACK);
    assert!(stack >= requested && stack - requested < page);
    assert!(stack <= MAX_STACK);
    assert!(0 < GUARD && GUARD < top && top < mapping);
    assert_eq!(top - GUARD, stack);
    assert_eq!(mapping - top, GUARD);
    assert_eq!(GUARD % page, 0);
    assert_eq!(top % page, 0);
    assert_eq!(mapping % page, 0);
    assert_eq!(top % 16, 0);
}

#[cfg(kani)]
#[kani::proof]
fn invalid_task_page_sizes_are_rejected_before_rounding() {
    let bytes: usize = kani::any();
    let page: usize = kani::any();
    kani::assume(matches!(page, 0 | 3 | 12288 | 524288 | usize::MAX));
    assert_eq!(stack_layout(bytes, page), None);
}
