//! Driver binary consolidating the rfc0090 conformance tests into one test
//! binary. Each module below was formerly its own `tests/rfc0090_*.rs` file;
//! collapsing them into one crate cuts merge-gate compile + discovery cost.
//! Files live in `tests/rfc0090/` (a subdir is not auto-compiled as its own
//! binary) and are attached here via `#[path]` since a test crate root
//! resolves bare `mod` names against `tests/`, not the subdir.
#[path = "rfc0090/async_loan_tail.rs"]
mod async_loan_tail;
#[path = "rfc0090/indirect_tail.rs"]
mod indirect_tail;
#[path = "rfc0090/reference_tail_results.rs"]
mod reference_tail_results;
#[path = "rfc0090/reference_tail.rs"]
mod reference_tail;
#[path = "rfc0090/tail_positions.rs"]
mod tail_positions;
#[path = "rfc0090/var_tail_envelope.rs"]
mod var_tail_envelope;
