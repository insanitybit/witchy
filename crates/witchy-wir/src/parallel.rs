//! Per-function module work on several threads.
//!
//! A SafeWIR verdict and an admission plan are each a pure function of one
//! function and shared module inputs (see `canonical_safewir::check::cache`),
//! so functions are processed independently and their results are consumed
//! in module order afterwards: the first error reported, and every
//! module-level check that follows, are exactly the sequential ones.
//! `WITCHY_CHECK_THREADS` sets the worker count; with `1` the callers work on
//! the calling thread, in order. The repo's cargo config sets it to `1` for
//! test processes, which already run in parallel with each other.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Modules with fewer functions are checked on the calling thread.
const MIN_PARALLEL_FUNCTIONS: usize = 32;
const MAX_WORKERS: usize = 8;
/// The checker recurses over nested regions; give workers the test threads'
/// stack (`RUST_MIN_STACK` in `.cargo/config.toml`), above the main thread's.
const WORKER_STACK: usize = 16 * 1024 * 1024;

pub(crate) fn workers(functions: usize) -> usize {
    static CONFIGURED: OnceLock<usize> = OnceLock::new();
    let configured = *CONFIGURED.get_or_init(|| {
        std::env::var("WITCHY_CHECK_THREADS").ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|&workers| workers > 0)
            .unwrap_or_else(|| {
                std::thread::available_parallelism().map_or(1, usize::from).min(MAX_WORKERS)
            })
    });
    if functions < MIN_PARALLEL_FUNCTIONS { 1 } else { configured.min(functions) }
}

/// `f` of every item, in item order, computed on `workers` threads.
pub(crate) fn map_ordered<T: Sync, R: Send>(
    items: &[T],
    workers: usize,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let mut results = std::thread::scope(|scope| {
        let handles = (0..workers).map(|_| {
            std::thread::Builder::new().stack_size(WORKER_STACK).spawn_scoped(scope, || {
                let mut done = Vec::new();
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else { break };
                    done.push((index, f(item)));
                }
                done
            }).expect("spawn a checker worker")
        }).collect::<Vec<_>>();
        handles.into_iter()
            .flat_map(|handle| handle.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic)))
            .collect::<Vec<_>>()
    });
    results.sort_unstable_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, result)| result).collect()
}
