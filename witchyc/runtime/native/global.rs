//! Process state read and written by the runtime's one thread.

use core::cell::Cell;

/// A value held for the life of the process. It is copied in and out, so no
/// reference to it exists to outlive a write.
pub struct Global<T>(Cell<T>);

// SAFETY: this holds for the whole runtime, not for this type alone. The runtime
// starts no thread, a forked child has its own copy, and no signal handler it
// installs reaches a `Global`. Starting a thread here breaks every use.
unsafe impl<T> Sync for Global<T> {}

impl<T: Copy> Global<T> {
    pub const fn new(value: T) -> Self {
        Self(Cell::new(value))
    }

    pub fn get(&self) -> T {
        self.0.get()
    }

    pub fn set(&self, value: T) {
        self.0.set(value)
    }
}
