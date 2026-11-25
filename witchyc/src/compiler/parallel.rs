//! Work on independent items spread over a few threads, with results in the items' order.

/// At most this many threads, the calling one included (callers run on a `STACK` thread): proof statements for a program are a
/// few milliseconds of work, and more threads cost more to start than they save.
const THREADS: usize = 4;

/// `items.iter().map(work)`, computed on up to `THREADS` threads. The order of the results is the
/// order of the items, whatever order the threads finish in.
pub(super) fn map<T: Sync, R: Send>(items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(THREADS).min(items.len());
    if threads <= 1 { return items.iter().map(&work).collect(); }
    // Each thread takes the next item not yet taken, so one large item does not hold up a
    // share of the others behind it. Results are put back by position.
    let next = std::sync::atomic::AtomicUsize::new(0);
    let take = || {
        let mut done = Vec::new();
        loop {
            let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let Some(item) = items.get(at) else { return done };
            done.push((at, work(item)));
        }
    };
    let mut slots: Vec<Option<R>> = std::iter::repeat_with(|| None).take(items.len()).collect();
    std::thread::scope(|scope| {
        let others = (1..threads).map(|_| spawn(scope, &take)).collect::<Vec<_>>();
        for (at, result) in take() { slots[at] = Some(result); }
        for handle in others {
            for (at, result) in handle.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic)) { slots[at] = Some(result); }
        }
    });
    slots.into_iter().map(|slot| slot.expect("every item was taken once")).collect()
}

/// The stack every model thread gets. The models recurse over expressions and paths up to
/// their own complexity limits, which must be what stops them, never the stack; a thread
/// reserves this much address space and touches only what it uses.
pub(super) const STACK: usize = 256 << 20;

/// A scoped thread with `STACK` of stack.
pub(super) fn spawn<'scope, 'env, T: Send + 'scope>(scope: &'scope std::thread::Scope<'scope, 'env>, work: impl FnOnce() -> T + Send + 'scope) -> std::thread::ScopedJoinHandle<'scope, T> {
    std::thread::Builder::new().stack_size(STACK).spawn_scoped(scope, work).expect("a model thread starts")
}

#[cfg(test)]
mod tests {
    #[test]
    fn results_keep_the_order_of_the_items() {
        let items = (0..37u64).collect::<Vec<_>>();
        assert_eq!(super::map(&items, |n| n * n), items.iter().map(|n| n * n).collect::<Vec<_>>());
        assert_eq!(super::map(&[] as &[u64], |n| *n), Vec::<u64>::new());
        assert_eq!(super::map(&[5u64], |n| n + 1), vec![6]);
    }
}
