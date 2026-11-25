//! Witchy frontend and compiler.
pub mod ast;
pub mod diagnostic;
pub mod lexer;
pub mod parser;
pub mod token;

/// Shared checking, proof admission, and target backends.
pub mod compiler;
pub use compiler as wasm;

#[cfg(test)]
#[path = "../runtime/native/budget.rs"]
mod native_budget;

#[cfg(test)]
#[path = "../runtime/native/exec_deadline.rs"]
mod native_exec_deadline;

#[cfg(test)]
#[path = "../runtime/native/heap_layout.rs"]
mod native_heap_layout;

#[cfg(test)]
#[path = "../runtime/native/heap_slots.rs"]
mod native_heap_slots;

#[cfg(test)]
#[path = "../runtime/native/heap_range.rs"]
mod native_heap_range;

#[cfg(test)]
#[path = "../runtime/native/grant_policy.rs"]
mod native_grant_policy;

#[cfg(test)]
#[path = "../runtime/native/launch_option.rs"]
mod native_launch_option;

#[cfg(all(test, target_arch = "aarch64"))]
use {native_heap_layout as heap_layout, native_heap_range as heap_range, native_heap_slots as heap_slots};

#[cfg(all(test, target_arch = "aarch64"))]
#[path = "../runtime/native/heap.rs"]
mod native_heap;

#[cfg(test)]
#[path = "../runtime/native/policy_grants.rs"]
mod native_policy_grants;

#[cfg(test)]
#[path = "../runtime/native/env_names.rs"]
mod native_env_names;

#[cfg(test)]
#[path = "../runtime/native/console.rs"]
mod native_console;

#[cfg(test)]
#[path = "../runtime/native/file_identity.rs"]
mod native_file_identity;

#[cfg(test)]
#[path = "../runtime/native/broker_path.rs"]
mod native_broker_path;
