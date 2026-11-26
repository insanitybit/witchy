//! Process deadline setup for the launcher and forked workers.

use super::global::Global;
use core::{ffi::c_int, ptr};

const SIGALRM: c_int = 14;
#[cfg(target_os = "macos")]
const SIG_UNBLOCK: c_int = 2;
#[cfg(target_os = "linux")]
const SIG_UNBLOCK: c_int = 1;
const ITIMER_REAL: c_int = 0;

static DEADLINE_MS: Global<u64> = Global::new(0);

pub(super) fn left_ms() -> Option<u64> {
    let end = DEADLINE_MS.get();
    (end != 0).then(|| end.saturating_sub(monotonic_ms().unwrap_or(u64::MAX)))
}

#[cfg(target_os = "macos")]
pub(super) fn monotonic_ms() -> Option<u64> {
    unsafe extern "C" {
        safe fn clock_gettime_nsec_np(clock: c_int) -> u64;
    }
    Some(clock_gettime_nsec_np(8) / 1_000_000)
}

#[cfg(target_os = "linux")]
pub(super) fn monotonic_ms() -> Option<u64> {
    use super::clock_random::Timespec;
    unsafe extern "C" {
        fn clock_gettime(clock: c_int, out: *mut Timespec) -> c_int;
    }
    let mut time = Timespec {
        seconds: 0,
        nanos: 0,
    };
    // SAFETY: the call writes one `Timespec`, and `time` is one.
    if unsafe { clock_gettime(1, &mut time) } != 0 {
        return None;
    }
    if time.seconds < 0 || !(0..1_000_000_000).contains(&time.nanos) {
        return None;
    }
    Some(
        (time.seconds as u64)
            .saturating_mul(1000)
            .saturating_add(time.nanos as u64 / 1_000_000),
    )
}

#[cfg(target_os = "macos")]
pub(super) type SignalSet = u32;
#[cfg(target_os = "linux")]
#[repr(C)]
pub(super) struct SignalSet {
    words: [u64; 16],
}

#[cfg(target_os = "macos")]
pub(super) fn empty_signal_set() -> SignalSet {
    0
}
#[cfg(target_os = "linux")]
pub(super) fn empty_signal_set() -> SignalSet {
    SignalSet { words: [0; 16] }
}

#[repr(C)]
pub(super) struct SignalAction {
    pub(super) handler: usize,
    pub(super) mask: SignalSet,
    pub(super) flags: c_int,
    #[cfg(target_os = "linux")]
    restorer: usize,
}

impl SignalAction {
    pub(super) fn new(handler: usize) -> Self {
        Self {
            handler,
            mask: empty_signal_set(),
            flags: 0,
            #[cfg(target_os = "linux")]
            restorer: 0,
        }
    }
}

#[repr(C)]
struct Timeval {
    seconds: i64,
    #[cfg(target_os = "macos")]
    micros: i32,
    #[cfg(target_os = "linux")]
    micros: i64,
}

#[repr(C)]
struct Timer {
    interval: Timeval,
    value: Timeval,
}

const _: () = {
    use core::mem::offset_of;
    assert!(size_of::<Timeval>() == 16 && align_of::<Timeval>() == 8);
    assert!(offset_of!(Timeval, micros) == 8);
    assert!(size_of::<Timer>() == 32 && align_of::<Timer>() == 8);
    assert!(offset_of!(Timer, value) == 16);
    assert!(offset_of!(SignalAction, handler) == 0);
    assert!(offset_of!(SignalAction, mask) == 8);
    assert!(align_of::<SignalAction>() == 8);
    #[cfg(target_os = "macos")]
    {
        assert!(size_of::<SignalSet>() == 4 && align_of::<SignalSet>() == 4);
        assert!(size_of::<SignalAction>() == 16);
        assert!(offset_of!(SignalAction, flags) == 12);
    }
    #[cfg(target_os = "linux")]
    {
        assert!(size_of::<SignalSet>() == 128 && align_of::<SignalSet>() == 8);
        assert!(size_of::<SignalAction>() == 152);
        assert!(offset_of!(SignalAction, flags) == 136);
        assert!(offset_of!(SignalAction, restorer) == 144);
    }
};

unsafe extern "C" {
    fn sigaction(signal: c_int, action: *const SignalAction, old: *mut SignalAction) -> c_int;
    fn sigemptyset(set: *mut SignalSet) -> c_int;
    fn sigaddset(set: *mut SignalSet, signal: c_int) -> c_int;
    fn sigprocmask(how: c_int, set: *const SignalSet, old: *mut SignalSet) -> c_int;
    fn setitimer(which: c_int, value: *const Timer, old: *mut Timer) -> c_int;
}

/// The status a socket-entry worker exits with when its setup bound expires
/// before its guest code starts.
pub(super) const SETUP_TIMED_OUT: c_int = 124;

#[cfg(target_os = "macos")]
extern "C" fn setup_expired(_: c_int) {
    unsafe extern "C" {
        safe fn _exit(code: c_int) -> !;
    }
    _exit(SETUP_TIMED_OUT)
}

/// Bounds a socket-entry worker's setup (its sandbox install) by `milliseconds`.
/// At expiry the worker exits with `SETUP_TIMED_OUT`, so the supervisor can tell
/// a setup that never reached guest code from a connection deadline. The caller
/// re-arms with `witchy_arm_deadline` once setup is done, which restores the
/// default action before guest code runs.
#[cfg(target_os = "macos")]
pub(super) fn arm_setup_deadline(milliseconds: u64) -> bool {
    arm(milliseconds, setup_expired as *const () as usize)
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn witchy_arm_deadline(milliseconds: u64) -> bool {
    arm(milliseconds, 0)
}

fn arm(milliseconds: u64, handler: usize) -> bool {
    const SIG_IGN: usize = 1;
    let mut action = SignalAction::new(handler);
    let mut ignore = SignalAction::new(SIG_IGN);
    let mut alarm = empty_signal_set();
    let off = Timer {
        interval: Timeval {
            seconds: 0,
            micros: 0,
        },
        value: Timeval {
            seconds: 0,
            micros: 0,
        },
    };
    // Replace an earlier timer and its handler with no window in which the earlier timer's
    // signal meets the new handler (or the default action meets a setup timer's signal):
    // stop the earlier timer, discard a signal it left pending (ignoring a signal discards
    // it), install the handler, and only then start the new timer.
    if unsafe {
        setitimer(ITIMER_REAL, &off, ptr::null_mut()) != 0
            || sigemptyset(&mut ignore.mask) != 0
            || sigaction(SIGALRM, &ignore, ptr::null_mut()) != 0
            || sigemptyset(&mut action.mask) != 0
            || sigaction(SIGALRM, &action, ptr::null_mut()) != 0
            || sigemptyset(&mut alarm) != 0
            || sigaddset(&mut alarm, SIGALRM) != 0
            || sigprocmask(SIG_UNBLOCK, &alarm, ptr::null_mut()) != 0
    } {
        return false;
    }
    let timer = Timer {
        interval: Timeval {
            seconds: 0,
            micros: 0,
        },
        value: Timeval {
            seconds: (milliseconds / 1000) as i64,
            micros: ((milliseconds % 1000) * 1000) as _,
        },
    };
    let armed = unsafe { setitimer(ITIMER_REAL, &timer, ptr::null_mut()) == 0 };
    if armed {
        let now = if milliseconds == 0 {
            0
        } else if let Some(now) = monotonic_ms() {
            now
        } else {
            return false;
        };
        DEADLINE_MS.set(if milliseconds == 0 {
            0
        } else {
            now.saturating_add(milliseconds)
        });
    }
    armed
}

#[unsafe(no_mangle)]
extern "C" fn witchy_run_deadline_ns(now: u64, milliseconds: u64) -> u64 {
    if milliseconds == 0 {
        0
    } else {
        now.saturating_add(milliseconds.saturating_mul(1_000_000))
    }
}

/// Returns zero at expiration and MAX for an unlimited run.
#[unsafe(no_mangle)]
extern "C" fn witchy_run_deadline_left_ms(end: u64, now: u64) -> u64 {
    if end == 0 {
        u64::MAX
    } else {
        end.saturating_sub(now).div_ceil(1_000_000)
    }
}
