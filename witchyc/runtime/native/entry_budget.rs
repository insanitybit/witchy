//! Entry creation consumes both the run-wide and connection-wide budgets.

// Set by the single-threaded connection broker after it forks.
use super::global::Global;
static CONNECTION: Global<u64> = Global::new(u64::MAX);

pub(super) fn set_connection(limit: u64) {
    CONNECTION.set(limit);
}

pub(super) fn available(run: u64) -> bool {
    run != 0 && CONNECTION.get() != 0
}

pub(super) fn limited(run: u64) -> bool {
    run != u64::MAX || CONNECTION.get() != u64::MAX
}

pub(super) fn spend(run: &mut u64) {
    if *run != u64::MAX {
        *run -= 1;
    }
    if CONNECTION.get() != u64::MAX {
        CONNECTION.set(CONNECTION.get() - 1);
    }
}

#[cfg(test)]
#[test]
fn both_budgets_limit_creation() {
    set_connection(1);
    let mut run = 3;
    assert!(limited(run) && available(run));
    spend(&mut run);
    assert_eq!(run, 2);
    assert!(!available(run));
    set_connection(u64::MAX);
    assert!(available(run));
    set_connection(u64::MAX);
}
