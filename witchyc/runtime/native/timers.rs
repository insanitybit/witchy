//! Native timer handles and deadlines. Timers are polled on the guest thread.

// A proof checks a table of eight places: the solver does not finish 4096.
const SLOTS: usize = if cfg!(kani) { 8 } else { 4096 };
const TIMER_BIT: u64 = 1 << 48;
const CLOCK: u64 = 16;

use super::global::State;

#[derive(Clone, Copy)]
struct Timer {
    handle: u64,
    due: u64,
}

struct Timers {
    slots: [Timer; SLOTS],
    sequence: u64,
    next: usize,
}
// Each use below is one search of the table: it calls nothing outside this
// file, so no use can begin inside another.
static TIMERS: State<Timers> = State::new(Timers {
    slots: [Timer { handle: 0, due: 0 }; SLOTS],
    sequence: 0,
    next: 0,
});

impl Timers {
    fn insert(&mut self, due: u64) -> Option<u64> {
        if self.sequence >= TIMER_BIT / SLOTS as u64 {
            return None;
        }
        for _ in 0..SLOTS {
            let index = self.next;
            self.next = (index + 1) % SLOTS;
            let timer = self.slots.get_mut(index)?;
            if timer.handle == 0 {
                let handle = TIMER_BIT + self.sequence * SLOTS as u64 + index as u64;
                self.sequence += 1;
                *timer = Timer { handle, due };
                return Some(handle);
            }
        }
        None
    }

    fn find(&mut self, handle: u64) -> Option<&mut Timer> {
        let timer = self.slots.get_mut(handle as usize % SLOTS)?;
        (handle != 0 && timer.handle == handle).then_some(timer)
    }
}

fn trap() -> ! {
    unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_clock_timer(grant: u64, due: u64) -> u64 {
    if grant != CLOCK {
        trap()
    }
    match TIMERS.with(|timers| timers.insert(due)) {
        Some(handle) => handle,
        None => trap(),
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_clock_timer_end(grant: u64, handle: u64) {
    if grant != CLOCK {
        trap()
    }
    let ended = TIMERS.with(|timers| timers.find(handle).map(|timer| timer.handle = 0));
    if ended.is_none() {
        trap()
    }
}

/// The launcher provides one writable deadline output.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_timer_due(handle: u64, due: *mut u64) -> bool {
    let Some(found) = TIMERS.with(|timers| timers.find(handle).map(|timer| timer.due)) else {
        return false;
    };
    // SAFETY: the launcher's promise: `due` is one writable deadline.
    unsafe { due.write(found) };
    true
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Any table at all, with the search starting inside it.
    fn any_timers() -> Timers {
        let mut timers = Timers {
            slots: [Timer { handle: 0, due: 0 }; SLOTS],
            sequence: kani::any(),
            next: kani::any(),
        };
        for timer in &mut timers.slots {
            *timer = Timer {
                handle: kani::any(),
                due: kani::any(),
            };
        }
        kani::assume(timers.next < SLOTS);
        timers
    }

    /// A handle finds only the timer that holds it, in the place it names; no
    /// handle reads outside the table.
    #[kani::proof]
    fn a_handle_finds_only_its_own_timer() {
        let mut timers = any_timers();
        let handle: u64 = kani::any();
        let held = timers.slots[handle as usize % SLOTS];
        let found = timers.find(handle).map(|timer| (timer.handle, timer.due));
        kani::cover!(found.is_some());
        kani::cover!(found.is_none() && handle == 0);
        assert_eq!(found.is_some(), handle != 0 && held.handle == handle);
        if let Some(found) = found {
            assert!(found == (handle, held.due));
        }
    }

    /// A new timer takes the first free place from `next`, and there is none
    /// only when the table is full or the handles have run out. Its handle is
    /// marked as a timer, names that place, carries the sequence number, and
    /// finds the deadline. No other place changes.
    #[kani::proof]
    fn a_new_timer_takes_the_first_free_place() {
        let mut timers = any_timers();
        let before = timers.slots;
        let (next, sequence) = (timers.next, timers.sequence);
        let due: u64 = kani::any();
        let handle = timers.insert(due);
        let other: usize = kani::any();
        kani::assume(other < SLOTS);
        let Some(handle) = handle else {
            kani::cover!(sequence == 0, "the table is full");
            kani::cover!(before[next].handle == 0, "handles run out");
            assert!(sequence >= TIMER_BIT / SLOTS as u64 || before[other].handle != 0);
            assert!(timers.slots[other].handle == before[other].handle);
            return;
        };
        let place = handle as usize % SLOTS;
        kani::cover!(place < next, "the search wraps");
        assert!(sequence < TIMER_BIT / SLOTS as u64 && before[place].handle == 0);
        assert!(TIMER_BIT <= handle && handle < 2 * TIMER_BIT);
        assert!((handle - TIMER_BIT) / SLOTS as u64 == sequence);
        assert!(timers.sequence == sequence + 1 && timers.next == (place + 1) % SLOTS);
        // Every place from `next` up to the one taken was in use.
        let passed = (other + SLOTS - next) % SLOTS < (place + SLOTS - next) % SLOTS;
        if passed {
            assert!(before[other].handle != 0);
        }
        if other != place {
            assert!(timers.slots[other].handle == before[other].handle);
            assert!(timers.slots[other].due == before[other].due);
        }
        let found = timers.find(handle).map(|timer| timer.due);
        assert!(found == Some(due));
    }
}
