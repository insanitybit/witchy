//! Model-check the same platform-independent modules used by the runtime.

#[path = "../../../runtime/native/budget.rs"]
mod budget;
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
