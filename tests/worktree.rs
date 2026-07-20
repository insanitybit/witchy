//! Driver binary consolidating the worktree integration tests into one test
//! binary. Each module below was formerly its own `tests/worktree_*.rs` file;
//! collapsing them into one crate cuts merge-gate compile + discovery cost.
//! Files live in `tests/worktree/` (a subdir is not auto-compiled as its own
//! binary) and are attached here via `#[path]` since a test crate root
//! resolves bare `mod` names against `tests/`, not the subdir.
#[path = "worktree/create.rs"]
mod create;
#[path = "worktree/status.rs"]
mod status;
#[path = "worktree/rfc_status.rs"]
mod rfc_status;
#[path = "worktree/warm.rs"]
mod warm;
