//! Identity and reuse policy for native heap slots.

/// A slot's size and its generation's offset, for a slot of a heap buffer pointer (heap.rs checks them).
pub const SLOT_BYTES: usize = core::mem::size_of::<Slot<core::ptr::NonNull<u8>>>();
pub const SLOT_GENERATION: usize = core::mem::offset_of!(Slot<core::ptr::NonNull<u8>>, generation);

// The layout is fixed: native code compiled from proved cell accesses reads a slot in place
// (value at 0, generation at 8; src/compiler/native/body/cells.rs).
#[derive(Clone, Copy)]
#[repr(C)]
struct Slot<T> {
    value: Option<T>,
    generation: u32,
    next: u32,
}

impl<T: Copy> Slot<T> {
    const EMPTY: Self = Self {
        value: None,
        generation: 0,
        next: 0,
    };
}

#[repr(C)]
pub struct Slots<T, const N: usize> {
    entries: [Slot<T>; N],
    used: u32,
    free: u32,
}

impl<T: Copy, const N: usize> Slots<T, N> {
    pub const fn new() -> Self {
        Self {
            entries: [Slot::EMPTY; N],
            used: 0,
            free: 0,
        }
    }

    pub fn has_room(&self) -> bool {
        self.free != 0 || (self.used as usize) < N
    }

    pub fn insert(&mut self, value: T) -> Option<u64> {
        let id = if self.free == 0 {
            self.used.checked_add(1)?
        } else {
            self.free
        };
        let slot = self.entries.get_mut(id.wrapping_sub(1) as usize)?;
        let generation = slot.generation.checked_add(1)?;
        if self.free == 0 {
            self.used = id;
        } else {
            self.free = slot.next;
        }
        slot.generation = generation;
        slot.value = Some(value);
        Some((u64::from(generation) << 32) | u64::from(id))
    }

    pub fn get(&self, handle: u64) -> Option<T> {
        let slot = self.entries.get((handle as u32).wrapping_sub(1) as usize)?;
        if slot.generation != (handle >> 32) as u32 {
            return None;
        }
        slot.value
    }

    pub fn remove(&mut self, handle: u64) -> Option<T> {
        let id = handle as u32;
        let slot = self.entries.get_mut(id.wrapping_sub(1) as usize)?;
        if slot.generation != (handle >> 32) as u32 {
            return None;
        }
        let value = slot.value.take()?;
        if slot.generation != u32::MAX {
            slot.next = self.free;
            self.free = id;
        }
        Some(value)
    }
}

#[cfg(test)]
#[test]
fn reuse_rejects_stale_handles_and_retires_exhausted_slots() {
    let mut slots = Slots::<u64, 1>::new();
    assert!(slots.has_room());
    let first = slots.insert(42).unwrap();
    assert!(!slots.has_room());
    assert_eq!(slots.remove(first), Some(42));
    let second = slots.insert(43).unwrap();
    assert_eq!(slots.get(first), None);
    assert_eq!(slots.remove(first), None);
    assert_eq!(slots.get(second), Some(43));
    assert_eq!(slots.remove(second), Some(43));
    slots.entries[0].generation = u32::MAX - 1;
    let last = slots.insert(44).unwrap();
    assert_eq!(slots.remove(last), Some(44));
    assert_eq!(slots.insert(45), None);
    assert!(!slots.has_room());
    assert_eq!(slots.get(last), None);
}

#[cfg(kani)]
#[kani::proof]
fn reuse_never_revives_the_previous_handle() {
    let mut slots = Slots::<u8, 1>::new();
    let generation: u32 = kani::any();
    kani::assume(generation < u32::MAX);
    slots.entries[0].generation = generation;
    let old = slots.insert(1).unwrap();
    assert_ne!(old >> 32, 0);
    assert_eq!(slots.remove(old), Some(1));
    if let Some(new) = slots.insert(2) {
        assert_ne!(new >> 32, 0);
        assert_ne!(old, new);
        assert_eq!(slots.remove(old), None);
        assert_eq!(slots.get(new), Some(2));
    } else {
        assert_eq!(generation, u32::MAX - 1);
        assert!(!slots.has_room());
    }
    assert_eq!(slots.get(old), None);
}

#[cfg(kani)]
#[kani::proof]
fn reuse_does_not_invalidate_another_live_slot() {
    let mut slots = Slots::<u8, 2>::new();
    let generation: u32 = kani::any();
    kani::assume(generation < u32::MAX);
    slots.entries[0].generation = generation;
    let first = slots.insert(1).unwrap();
    let second = slots.insert(2).unwrap();
    assert_ne!(first, second);
    assert_eq!(slots.remove(first), Some(1));
    if let Some(replacement) = slots.insert(3) {
        assert_ne!(first, replacement);
        assert_ne!(second, replacement);
        assert_eq!(slots.get(replacement), Some(3));
    } else {
        assert_eq!(generation, u32::MAX - 1);
    }
    assert_eq!(slots.get(first), None);
    assert_eq!(slots.remove(first), None);
    assert_eq!(slots.get(second), Some(2));
}
