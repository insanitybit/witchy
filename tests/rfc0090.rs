//! Driver binary consolidating the rfc0090 conformance tests into one test
//! binary. Each module below was formerly its own `tests/rfc0090_*.rs` file;
//! collapsing them into one crate cuts merge-gate compile + discovery cost.
//! Files live in `tests/rfc0090/` (a subdir is not auto-compiled as its own
//! binary) and are attached here via `#[path]` since a test crate root
//! resolves bare `mod` names against `tests/`, not the subdir.
#[path = "rfc0090/tail_positions.rs"]
mod tail_positions;
