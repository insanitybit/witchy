//! Worktree script tests live in the lightweight tooling package so changing
//! shell-only infrastructure does not compile or link the Witchy compiler.
#[path = "worktree/support.rs"]
mod support;
#[path = "worktree/create.rs"]
mod create;
#[path = "worktree/status.rs"]
mod status;
#[path = "worktree/rfc_status.rs"]
mod rfc_status;
#[path = "worktree/warm.rs"]
mod warm;
