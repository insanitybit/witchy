//! The single audited gateway for reading and mutating this process's environment.
//!
//! Mutating the environment is `unsafe` under edition 2024 for one specific
//! reason: `std::env::set_var`/`remove_var` write a process-global table that
//! `getenv` readers may be walking concurrently, in ANY thread — including threads
//! inside C libraries that never touch Rust. The result is a torn read or a
//! use-after-free, not a tidy panic.
//!
//! The launch paths that strip secret-bearing variables (RFC-0013 grant
//! documents, RFC-0092 trusted-executable plans) satisfy the requirement because
//! they run before the VM is instantiated and before the runtime spawns any
//! thread. That was previously asserted only in a comment — so this module makes
//! the claim CHECKED instead:
//!
//! - Every read and write goes through here, so "who touches the environment" is
//!   a closed, greppable set rather than a convention.
//! - A debug/test build takes a global lock and additionally verifies that this
//!   process is still single-threaded, aborting the run if it is not. A mutation
//!   attempted after the runtime spawns a thread therefore fails loudly in CI
//!   rather than corrupting memory in production.
//! - A release build keeps the lock (it is uncontended and off the hot path) and
//!   drops only the thread-count probe, which is the part that costs a syscall.
//!
//! The lock does NOT make mutation sound on its own — it cannot stop a `getenv`
//! in a thread that never acquires it. It serializes *our* accesses and gives the
//! thread check a place to live. Soundness still rests on mutating early; the
//! check is what stops that invariant from rotting silently.

use std::sync::{Mutex, MutexGuard, OnceLock};

/// Serializes this process's environment accesses. Poisoning is recovered from
/// deliberately: a panic elsewhere while holding this lock says nothing about the
/// environment table's integrity, and refusing to launch over it would be worse
/// than proceeding.
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Read a variable through the audited gateway.
///
/// Reads do not need the lock for soundness (concurrent `getenv` calls are fine
/// among themselves); taking it keeps every access in one place and means a read
/// cannot interleave with one of our own writes.
pub fn var(name: &str) -> Option<String> {
    let _guard = env_lock();
    std::env::var(name).ok()
}

/// Remove a variable from this process's environment. Returns whether it was set.
///
/// **Call only before the process becomes multi-threaded** — that is the whole
/// safety contract. Debug and test builds enforce it (see [`assert_single_threaded`]);
/// release builds trust it. Both take the lock.
pub fn remove_var(name: &str) -> bool {
    remove_var_checked(name, ThreadCheck::Enforce)
}

/// Whether a mutation must verify that this process is still single-threaded.
///
/// The launch paths always [`ThreadCheck::Enforce`]. Only a test harness — which
/// is multi-threaded by construction and therefore can never satisfy the real
/// contract — passes [`ThreadCheck::WaiveForTest`], and it can only obtain that
/// value with the `test-env-staging` feature enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadCheck {
    /// Verify (debug/test builds) that no other thread is live. Production.
    Enforce,
    /// Knowingly skip the verification. Test harnesses only.
    #[cfg(feature = "test-env-staging")]
    WaiveForTest,
}

/// Remove a variable, choosing whether the single-threaded check applies.
///
/// Exposed so a test can drive the REAL production code path (which strips
/// variables) without the harness's own threads tripping a check that exists to
/// protect production. The waiver is a visible argument at the call site rather
/// than an ambient mode, so a reader of that code sees exactly what was given up.
pub fn remove_var_checked(name: &str, check: ThreadCheck) -> bool {
    let _guard = env_lock();
    if check == ThreadCheck::Enforce {
        assert_single_threaded("remove_var", name);
    }
    let was_set = std::env::var(name).is_ok();
    // SAFETY: the environment table is mutated while holding `env_lock`, so no
    // OTHER access through this module can be in flight. The remaining hazard —
    // a `getenv` from a thread that does not use this module — is excluded by the
    // caller contract that mutation happens before any thread is spawned, which
    // `assert_single_threaded` verifies on every debug/test run.
    unsafe { std::env::remove_var(name) };
    was_set
}

/// Set a variable through the audited gateway. Same contract as [`remove_var`].
///
/// Production code has no reason to call this — the host reads the environment, it
/// does not publish to it. It exists so a test that must stage a variable shares
/// this lock instead of reaching for raw `unsafe`.
pub fn set_var(name: &str, value: &str) {
    let _guard = env_lock();
    assert_single_threaded("set_var", name);
    // SAFETY: as `remove_var` above.
    unsafe { std::env::set_var(name, value) };
}

/// Stage a variable from a TEST that is already running multi-threaded, bypassing
/// the single-threaded check.
///
/// A test harness runs many tests in parallel, so a test can never satisfy the
/// real contract — yet tests are exactly what must exercise env-backed secret
/// resolution. This is the sanctioned exception, and it is deliberately awkward:
/// gated behind the `test-env-staging` cargo feature that only `dev-dependencies`
/// turn on (so production builds cannot reach it at all), named for what it gives
/// up, and it still takes the lock so concurrent tests using this module do not
/// interleave writes with each other.
///
/// The residual risk is real but bounded: a `getenv` in an unrelated thread
/// (wasmtime's pool, a test's own spawn) could in principle race one of these
/// writes. Tests that stage secret variables must serialize on their own mutex —
/// see `witchy-caps`'s `grants` tests — and must use variable names no other test
/// reads.
#[cfg(feature = "test-env-staging")]
pub fn set_var_for_test(name: &str, value: &str) {
    let _guard = env_lock();
    // SAFETY: the lock excludes other accesses through this module. The
    // single-threaded contract is knowingly waived here — see the doc comment.
    unsafe { std::env::set_var(name, value) };
}

/// Remove a variable from a TEST that is already multi-threaded. The counterpart
/// to [`set_var_for_test`]; the same waiver and the same obligations apply.
#[cfg(feature = "test-env-staging")]
pub fn remove_var_for_test(name: &str) -> bool {
    let _guard = env_lock();
    let was_set = std::env::var(name).is_ok();
    // SAFETY: as `set_var_for_test`.
    unsafe { std::env::remove_var(name) };
    was_set
}

/// Abort if this process already has more than one thread.
///
/// This is the teeth behind the safety comments. It is a debug/test-only check
/// because it costs a platform query per call, and because a release build that
/// has already violated the contract is past the point where a panic helps.
///
/// It reads the live thread count from the OS rather than tracking a flag: a
/// thread spawned by a dependency (wasmtime's compilation pool, a runtime
/// watchdog) counts exactly as much as one we spawned.
#[cfg(any(debug_assertions, test))]
fn assert_single_threaded(operation: &str, name: &str) {
    if let Some(threads) = thread_count()
        && threads > 1
    {
        panic!(
            "process_env::{operation}(`{name}`) ran with {threads} live threads — mutating the \
             environment is only sound before this process becomes multi-threaded, because a \
             `getenv` in another thread (including inside a C library) can read a freed entry. \
             Move the mutation earlier: environment stripping belongs in launch-time grant \
             resolution, before the VM is instantiated."
        );
    }
}

#[cfg(not(any(debug_assertions, test)))]
fn assert_single_threaded(_operation: &str, _name: &str) {}

/// This process's live thread count, or `None` where we cannot ask cheaply.
/// `None` means "cannot verify", and is deliberately treated as passing — the
/// check must never invent a failure on a platform it does not understand.
#[cfg(any(debug_assertions, test))]
fn thread_count() -> Option<usize> {
    #[cfg(target_os = "linux")]
    {
        // /proc/self/stat field 20 is `num_threads`.
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        // The comm field may contain spaces inside parens; count from after it.
        let tail = stat.rsplit_once(')')?.1;
        tail.split_whitespace().nth(17)?.parse().ok()
    }
    #[cfg(target_os = "macos")]
    {
        // Counting entries in the task's thread list needs mach APIs (unsafe FFI),
        // which would defeat the purpose of this module. Read the thread count
        // that `ps` reports instead — this is a debug-only assertion, so the cost
        // of one short-lived process is acceptable, and a failure to measure is
        // treated as "cannot verify".
        let out = std::process::Command::new("/bin/ps")
            .args(["-M", "-p", &std::process::id().to_string()])
            .output()
            .ok()?;
        // One header line, then one line per thread.
        let lines = String::from_utf8_lossy(&out.stdout).lines().count();
        lines.checked_sub(1).filter(|threads| *threads > 0)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gateway round-trips, and `remove_var` reports whether the variable was
    /// actually set — the launch path uses that to stay idempotent when two
    /// secrets share one provider.
    #[test]
    fn set_and_remove_round_trip_through_the_gateway() {
        // A test harness is multi-threaded, so the checked entry points cannot be
        // used here — that is the point of the check, and of the test-only waiver.
        set_var_for_test("WITCHY_TEST_ENV_GATEWAY", "value");
        assert_eq!(var("WITCHY_TEST_ENV_GATEWAY").as_deref(), Some("value"));
        assert!(remove_var_for_test("WITCHY_TEST_ENV_GATEWAY"), "it was set");
        assert_eq!(var("WITCHY_TEST_ENV_GATEWAY"), None);
        assert!(!remove_var_for_test("WITCHY_TEST_ENV_GATEWAY"), "already gone");
    }

    /// The single-threaded check must be able to MEASURE on this platform,
    /// otherwise it silently passes and the guard is decorative. If this fails on a
    /// new platform, teach `thread_count` about it rather than deleting the test.
    #[test]
    fn the_thread_check_can_measure_this_platform() {
        assert!(
            thread_count().is_some(),
            "thread_count() must work on a supported platform, or the guard cannot fire"
        );
    }

    /// The guard actually fires: mutate from a process that has a second live
    /// thread and the call must panic rather than proceed. Verifying this needs a
    /// real extra thread, and the panic must not be swallowed, so the mutation runs
    /// on a joinable thread whose result we inspect.
    #[test]
    fn mutating_with_a_live_thread_panics() {
        let keep_alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let flag = keep_alive.clone();
        let parked = std::thread::spawn(move || {
            while flag.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        });
        // Wait until the OS actually reports the second thread, so the test is not
        // racing the spawn.
        let mut observed = false;
        for _ in 0..200 {
            if thread_count().is_some_and(|threads| threads > 1) {
                observed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(observed, "the spawned thread never became visible to thread_count()");

        let attempt = std::panic::catch_unwind(|| set_var("WITCHY_TEST_ENV_RACE", "x"));
        keep_alive.store(false, std::sync::atomic::Ordering::Relaxed);
        parked.join().expect("the parked thread exits");

        let error = attempt.expect_err("mutating with a live thread must panic");
        let message = error
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| error.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        assert!(
            message.contains("live threads"),
            "the panic must explain the thread hazard: {message}"
        );
        // The blocked mutation must not have happened.
        assert_eq!(var("WITCHY_TEST_ENV_RACE"), None);
    }
}
