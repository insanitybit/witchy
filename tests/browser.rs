//! Driver binary consolidating the browser integration tests into one test
//! binary. Each module below was formerly its own `tests/browser_*.rs` file;
//! collapsing them into one crate cuts merge-gate compile + discovery cost.
//! Files live in `tests/browser/` (a subdir is not auto-compiled as its own
//! binary) and are attached here via `#[path]` since a test crate root
//! resolves bare `mod` names against `tests/`, not the subdir.
#[path = "browser/encoding.rs"]
mod encoding;
#[path = "browser/shim.rs"]
mod shim;
