//! Repository-contract tests that do not depend on the Witchy compiler.
//!
//! Keeping these in a lightweight workspace package lets documentation,
//! packaging, and architecture edits validate their contracts without first
//! compiling the native compiler graph.

use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR")
            .expect("Cargo sets CARGO_MANIFEST_DIR for each test process"),
    )
    .join("../..")
}

#[path = "repo_hygiene/architecture.rs"]
mod architecture;
#[path = "repo_hygiene/example_source_paths.rs"]
mod example_source_paths;
#[path = "repo_hygiene/release_packaging.rs"]
mod release_packaging;
#[path = "repo_hygiene/rfc_status_hygiene.rs"]
mod rfc_status_hygiene;
#[path = "repo_hygiene/spec_freshness.rs"]
mod spec_freshness;
