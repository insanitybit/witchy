//! Driver binary consolidating the rfc0082 conformance tests into one test
//! binary. Each module below was formerly its own `tests/rfc0082_*.rs` file;
//! collapsing them into one crate cuts merge-gate compile + link cost (three
//! separate binaries each linking the full witchy lib + wasmtime, to run a
//! combined ~570 lines of tests) and removes the redundant per-file compile
//! of the shared `support/authenticated.rs` helper. Files live in
//! `tests/rfc0082/` (a subdir is not auto-compiled as its own binary) and are
//! attached here via `#[path]` since a test crate root resolves bare `mod`
//! names against `tests/`, not the subdir.
#[path = "support/authenticated.rs"]
mod authenticated;
#[path = "rfc0082/dynamic.rs"]
mod dynamic;
#[path = "rfc0082/dynamic_methods.rs"]
mod dynamic_methods;
#[path = "rfc0082/ownership.rs"]
mod ownership;
