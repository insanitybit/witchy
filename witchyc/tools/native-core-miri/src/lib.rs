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
