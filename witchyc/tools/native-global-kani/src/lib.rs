//! Kani checks of the runtime's process-state cells (`runtime/native/global.rs`).
#![no_std]
#![allow(dead_code)]

#[path = "../../../runtime/native/global.rs"]
mod global;

#[cfg(kani)]
mod proofs {
    use super::global::{Global, State, Table};

    const PLACES: usize = 4;
    const STEPS: usize = 6;

    #[kani::proof]
    fn a_global_holds_the_last_value_set() {
        let first: u64 = kani::any();
        let second: u64 = kani::any();
        let global = Global::new(first);
        assert_eq!(global.get(), first);
        global.set(second);
        assert_eq!(global.get(), second);
    }

    /// Any run of pushes and clears leaves the table agreeing with a plain
    /// array and count kept beside it, and never past its last place.
    #[kani::proof]
    #[kani::unwind(8)]
    fn a_table_agrees_with_an_array_and_count() {
        let table: Table<u64, PLACES> = Table::new(0);
        let mut model = [0u64; PLACES];
        let mut count = 0usize;
        for _ in 0..STEPS {
            if kani::any() {
                table.clear();
                count = 0;
            } else {
                let item: u64 = kani::any();
                let added = table.push(item);
                assert_eq!(added, count < PLACES);
                if added {
                    model[count] = item;
                    count += 1;
                }
            }
            assert_eq!(table.len(), count);
            assert_eq!(table.is_empty(), count == 0);
        }
        let index: usize = kani::any();
        match table.get(index) {
            Some(item) => {
                assert!(index < count);
                assert_eq!(item, model[index]);
            }
            None => assert!(index >= count),
        }
        let mut seen = 0;
        for item in table.iter() {
            assert_eq!(item, model[seen]);
            seen += 1;
        }
        assert_eq!(seen, count);
    }

    /// C code is handed `as_ptr()` and `len()`: every place below the length
    /// is readable there and holds the item pushed.
    #[kani::proof]
    #[kani::unwind(6)]
    fn the_pointer_reaches_every_item_in_use() {
        let table: Table<u64, PLACES> = Table::new(0);
        let pushes: usize = kani::any();
        kani::assume(pushes <= PLACES + 1);
        for _ in 0..pushes {
            table.push(kani::any());
        }
        let index: usize = kani::any();
        kani::assume(index < table.len());
        let read = unsafe { table.as_ptr().add(index).read() };
        assert_eq!(Some(read), table.get(index));
    }

    /// A change made inside one use is seen by the next, and a use returns
    /// what its closure does. Two different values may be in use together.
    #[kani::proof]
    fn a_state_keeps_what_a_use_writes() {
        let state = State::new([0u64; 2]);
        let other = State::new(0u8);
        let value: u64 = kani::any();
        let back = state.with(|pair| {
            pair[1] = value;
            other.with(|byte| *byte = 1);
            pair[0]
        });
        assert_eq!(back, 0);
        assert_eq!(state.with(|pair| pair[1]), value);
        assert_eq!(other.with(|byte| *byte), 1);
    }

    /// A second use of a value inside a first stops the process.
    #[kani::proof]
    #[kani::should_panic]
    fn a_state_in_use_refuses_a_second_use() {
        let state = State::new(0u64);
        state.with(|_| state.with(|inner| *inner = 1));
    }
}
