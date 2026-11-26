//! Miri exercises the same runtime core modules used by native programs.

#[path = "../../../runtime/native/broker_path.rs"]
mod broker_path;
#[path = "../../../runtime/native/budget.rs"]
mod budget;
#[path = "../../../runtime/native/console.rs"]
mod console;
#[path = "../../../runtime/native/env_names.rs"]
mod env_names;
#[path = "../../../runtime/native/file_identity.rs"]
mod file_identity;
#[path = "../../../runtime/native/grant_policy.rs"]
mod grant_policy;
#[path = "../../../runtime/native/heap.rs"]
mod heap;
#[path = "../../../runtime/native/heap_layout.rs"]
mod heap_layout;
#[path = "../../../runtime/native/heap_range.rs"]
mod heap_range;
#[path = "../../../runtime/native/heap_slots.rs"]
mod heap_slots;
#[path = "../../../runtime/native/launch_option.rs"]
mod launch_option;
#[path = "../../../runtime/native/policy_grants.rs"]
mod policy_grants;

#[path = "../../../runtime/native/global.rs"]
mod global;

#[test]
fn state_reentry_panics_before_lending_a_second_reference() {
    use std::{
        cell::Cell,
        panic::{AssertUnwindSafe, catch_unwind},
    };

    let state = global::State::new(0);
    let reentered = Cell::new(false);
    let result = catch_unwind(AssertUnwindSafe(|| {
        state.with(|outer| {
            *outer = 1;
            let nested = catch_unwind(AssertUnwindSafe(|| {
                state.with(|inner| {
                    reentered.set(true);
                    *inner = 2;
                });
            }));
            assert!(nested.is_err());
            assert!(!reentered.get());
            assert_eq!(*outer, 1);
            *outer = 3;
        });
    }));
    assert!(result.is_ok());
    assert_eq!(state.with(|value| *value), 3);
}

#[path = "../../../runtime/native/launch_args.rs"]
mod launch_args;

#[test]
fn launch_arguments_keep_each_selected_word_in_bounds() {
    unsafe extern "C" {
        fn witchy_args_count(grant: u64) -> u64;
        fn witchy_args_select(grant: u64, index: usize) -> u64;
        fn witchy_args_byte(grant: u64, index: usize) -> u64;
    }

    const GRANT: u64 = 64;
    const NO_BYTE: u64 = 256;
    assert!(launch_args::witchy_args_bind(c""));
    assert!(launch_args::witchy_args_bind(c"é"));
    assert!(!launch_args::witchy_args_bind(c"\xff"));
    assert!(launch_args::witchy_args_bind(c"z"));
    unsafe {
        assert_eq!(witchy_args_count(GRANT), 3);
        assert_eq!(witchy_args_select(GRANT, 0), 0);
        assert_eq!(witchy_args_byte(GRANT, 0), NO_BYTE);
        assert_eq!(witchy_args_select(GRANT, 1), 2);
        assert_eq!(witchy_args_byte(GRANT, 0), 0xC3);
        assert_eq!(witchy_args_byte(GRANT, 1), 0xA9);
        assert_eq!(witchy_args_byte(GRANT, 2), NO_BYTE);
        assert_eq!(witchy_args_select(GRANT, 2), 1);
        assert_eq!(witchy_args_byte(GRANT, 0), u64::from(b'z'));
        assert_eq!(witchy_args_byte(GRANT, usize::MAX), NO_BYTE);
        assert_eq!(witchy_args_select(GRANT, usize::MAX), u64::MAX);
        assert_eq!(witchy_args_byte(GRANT, 0), NO_BYTE);
    }
}

mod p384 {
    // A key's provider allocation is outside this slot-table test.
    pub(super) struct Key(pub(super) u64);
}

#[path = "../../../runtime/native/p384_slots.rs"]
mod p384_slots;

#[test]
fn p384_slot_guard_serializes_two_threads() {
    use std::sync::Barrier;

    let ready = Barrier::new(2);
    std::thread::scope(|threads| {
        for value in [11, 22] {
            let ready = &ready;
            threads.spawn(move || {
                ready.wait();
                let handle = p384_slots::with(|slots| {
                    let place = slots.available().unwrap();
                    slots.insert(place, p384::Key(value))
                });
                ready.wait();
                p384_slots::with(|slots| {
                    assert_eq!(slots.get(handle).unwrap().0, value);
                    assert_eq!(slots.take(handle).unwrap().0, value);
                    assert!(slots.get(handle).is_none());
                });
            });
        }
    });
}
