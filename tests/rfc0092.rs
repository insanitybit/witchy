//! Driver binary consolidating the rfc0092 conformance tests into one test
//! binary. Each module below was formerly its own `tests/rfc0092_*.rs` file;
//! collapsing them into one crate cuts merge-gate compile + discovery cost.
//! Files live in `tests/rfc0092/` (a subdir is not auto-compiled as its own
//! binary) and are attached here via `#[path]` since a test crate root
//! resolves bare `mod` names against `tests/`, not the subdir.
#[path = "rfc0092/trusted_application_executables.rs"]
mod trusted_application_executables;
#[path = "rfc0092/trusted_minigrep_distribution.rs"]
mod trusted_minigrep_distribution;
