//! Driver binary consolidating the glamour integration tests into one test
//! binary. Each module below was formerly its own `tests/glamour_*.rs` file;
//! collapsing them into one crate cuts merge-gate compile + discovery cost.
//! Files live in `tests/glamour/` (a subdir is not auto-compiled as its own
//! binary) and are attached here via `#[path]` since a test crate root
//! resolves bare `mod` names against `tests/`, not the subdir.
#[path = "glamour/dom.rs"]
mod dom;
#[path = "glamour/html_nul.rs"]
mod html_nul;
