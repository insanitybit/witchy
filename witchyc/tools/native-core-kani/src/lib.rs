//! Model-check the same platform-independent modules used by the runtime.

#[path = "../../../runtime/native/budget.rs"]
mod budget;
#[path = "../../../runtime/native/digest.rs"]
mod digest;
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

#[path = "../../../runtime/native/task_stack_layout.rs"]
mod task_stack_layout;

#[cfg(kani)]
mod p384 {
    // Only key ownership is modeled here. Provider operations have their own fixtures.
    pub(super) struct Key(pub(super) u64);
}
#[cfg(kani)]
#[path = "../../../runtime/native/p384_slots.rs"]
mod p384_slots;

#[cfg(kani)]
#[kani::proof]
#[kani::unwind(18)]
fn p384_slot_reuse_never_revives_the_taken_key() {
    use p384::Key;

    let old_value: u64 = kani::any();
    let new_value: u64 = kani::any();
    let (slot, old) = p384_slots::with(|slots| {
        let slot = slots.available().unwrap();
        (slot, slots.insert(slot, Key(old_value)))
    });
    assert_eq!(p384_slots::with(|slots| slots.take(old)).unwrap().0, old_value);

    let new = p384_slots::with(|slots| {
        assert_eq!(slots.available(), Some(slot));
        slots.insert(slot, Key(new_value))
    });
    assert_ne!(old, new);
    p384_slots::with(|slots| {
        assert!(slots.get(old).is_none());
        assert!(slots.take(old).is_none());
        assert_eq!(slots.get(new).unwrap().0, new_value);
        assert_eq!(slots.take(new).unwrap().0, new_value);
        assert!(slots.get(new).is_none());
    });
}

#[cfg(kani)]
#[path = "."]
mod tls_record_model {
    mod digest {
        // These imports are intentionally unresolved. The bounds proof checks
        // that rejected requests cannot reach either provider operation.
        unsafe extern "C" {
            pub(super) safe fn fips_mode() -> core::ffi::c_int;
            pub(super) safe fn clear_error();
        }
    }

    #[path = "../../../runtime/native/tls_record.rs"]
    mod tls_record;
}
