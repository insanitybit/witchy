//! End-to-end tests for coven, the witchy package manager. These drive the real
//! `witchy` binary (via `CARGO_BIN_EXE_witchy`) through the full supply-chain
//! lifecycle: scaffold, publish (staged), promote (second factor), add (gated),
//! build, run, audit. Each test is hermetic — its own temp `WITCHY_HOME` and
//! working tree — so they can run in parallel.

mod support;
#[path = "support/registry.rs"]
mod registry;
#[path = "support/package_manager.rs"]
mod package_manager;
#[path = "support/sandbox.rs"]
mod sandbox;

#[path = "e2e/trust_and_publishing.rs"]
mod trust_and_publishing;
#[path = "e2e/capability_widening.rs"]
mod capability_widening;
#[path = "e2e/resolution.rs"]
mod resolution;
#[path = "e2e/build_steps.rs"]
mod build_steps;
#[path = "e2e/example_workspaces.rs"]
mod example_workspaces;
#[path = "e2e/coven_web.rs"]
mod coven_web;
#[path = "e2e/pm_coven_lifecycle.rs"]
mod pm_coven_lifecycle;
#[path = "e2e/sandbox_grants.rs"]
mod sandbox_grants;

/// JSON-encode a string (quoted, with `"`, `\`, and newlines escaped).
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}
