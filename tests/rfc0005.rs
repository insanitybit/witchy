//! Driver binary consolidating the rfc0005 conformance tests into one test
//! binary. Each module below was formerly its own `tests/rfc0005_*.rs` file;
//! collapsing them into one crate cuts merge-gate compile + discovery cost.
//! Files live in `tests/rfc0005/` (a subdir is not auto-compiled as its own
//! binary) and are attached here via `#[path]` since a test crate root
//! resolves bare `mod` names against `tests/`, not the subdir.
#[path = "rfc0005/first_class_monomorphization_matrix.rs"]
mod first_class_monomorphization_matrix;
#[path = "rfc0005/first_class_monomorphization.rs"]
mod first_class_monomorphization;
#[path = "rfc0005/gc_let_pattern.rs"]
mod gc_let_pattern;
#[path = "rfc0005/typed_closure_abi.rs"]
mod typed_closure_abi;
