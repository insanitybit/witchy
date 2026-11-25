//! Per-process ownership of pending P-384 keys. Socket workers fork before
//! entering guest code and each worker starts with an empty table.

use core::{cell::UnsafeCell, sync::atomic::{AtomicBool, Ordering}};

use super::p384::Key;

const LIMIT: usize = 16;

struct Entry {
    handle: u64,
    key: Key,
}

pub(super) struct Slots {
    entries: [Option<Entry>; LIMIT],
    next: u64,
}

struct Pending {
    busy: AtomicBool,
    slots: UnsafeCell<Slots>,
}

// All access to the cell is serialized by the guard in with().
unsafe impl Sync for Pending {}

static PENDING: Pending = Pending {
    busy: AtomicBool::new(false),
    slots: UnsafeCell::new(Slots {
        entries: [const { None }; LIMIT],
        next: 256,
    }),
};

pub(super) fn with<T>(operation: impl FnOnce(&mut Slots) -> T) -> T {
    struct Unlock;
    impl Drop for Unlock {
        fn drop(&mut self) {
            PENDING.busy.store(false, Ordering::Release);
        }
    }
    while PENDING.busy.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        core::hint::spin_loop();
    }
    let _unlock = Unlock;
    operation(unsafe { &mut *PENDING.slots.get() })
}

impl Slots {
    pub(super) fn available(&self) -> Option<usize> {
        self.next.checked_add(1)?;
        self.entries.iter().position(Option::is_none)
    }

    pub(super) fn insert(&mut self, slot: usize, key: Key) -> u64 {
        let handle = self.next;
        self.entries[slot] = Some(Entry { handle, key });
        self.next += 1;
        handle
    }

    pub(super) fn get(&self, handle: u64) -> Option<&Key> {
        self.entries.iter().flatten().find(|entry| entry.handle == handle).map(|entry| &entry.key)
    }

    pub(super) fn take(&mut self, handle: u64) -> Option<Key> {
        let slot = self.entries.iter_mut().find(|slot| slot.as_ref().is_some_and(|entry| entry.handle == handle))?;
        slot.take().map(|entry| entry.key)
    }
}
