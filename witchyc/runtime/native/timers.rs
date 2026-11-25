//! Native timer handles and deadlines. Timers are polled on the guest thread.

const SLOTS: usize = 4096;
const TIMER_BIT: u64 = 1 << 48;
const CLOCK: u64 = 16;

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
static mut TIMERS: Timers = Timers {
    slots: [Timer { handle: 0, due: 0 }; SLOTS],
    sequence: 0,
    next: 0,
};

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
    let timers = unsafe { &mut *(&raw mut TIMERS) };
    match timers.insert(due) {
        Some(handle) => handle,
        None => trap(),
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_clock_timer_end(grant: u64, handle: u64) {
    if grant != CLOCK {
        trap()
    }
    let timers = unsafe { &mut *(&raw mut TIMERS) };
    let Some(timer) = timers.find(handle) else {
        trap()
    };
    timer.handle = 0;
}

/// The launcher provides one writable deadline output.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_timer_due(handle: u64, due: *mut u64) -> bool {
    let timers = unsafe { &mut *(&raw mut TIMERS) };
    let Some(timer) = timers.find(handle) else {
        return false;
    };
    unsafe { due.write(timer.due) };
    true
}
