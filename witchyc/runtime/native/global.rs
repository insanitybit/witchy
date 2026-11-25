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

/// A fixed number of places filled from the first, with a count of those in use.
pub struct Table<T, const N: usize> {
    items: Global<[T; N]>,
    count: Global<usize>,
}

impl<T: Copy, const N: usize> Table<T, N> {
    /// `empty` fills the places not yet in use; no reader sees it.
    pub const fn new(empty: T) -> Self {
        Self {
            items: Global::new([empty; N]),
            count: Global::new(0),
        }
    }

    fn cells(&self) -> &[Cell<T>] {
        let cells: &Cell<[T]> = &self.items.0;
        cells.as_slice_of_cells()
    }

    pub fn len(&self) -> usize {
        self.count.get()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        self.count.set(0)
    }

    /// False, and nothing is added, when every place is in use.
    pub fn push(&self, item: T) -> bool {
        let count = self.len();
        let Some(cell) = self.cells().get(count) else {
            return false;
        };
        cell.set(item);
        self.count.set(count + 1);
        true
    }

    pub fn get(&self, index: usize) -> Option<T> {
        self.cells()[..self.len()].get(index).map(Cell::get)
    }

    /// The items in use, each copied out as it is reached.
    pub fn iter(&self) -> impl Iterator<Item = T> + '_ {
        self.cells()[..self.len()].iter().map(Cell::get)
    }

    /// The first place, for C code that takes the table with `len()`.
    pub fn as_ptr(&self) -> *const T {
        self.cells().as_ptr().cast()
    }
}
