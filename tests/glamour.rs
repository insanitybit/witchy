//! Driver binary consolidating the glamour integration tests into one test
//! binary. Each module below was formerly its own `tests/glamour_*.rs` file;
//! collapsing them into one crate cuts merge-gate compile + discovery cost.
//! Files live in `tests/glamour/` (a subdir is not auto-compiled as its own
//! binary) and are attached here via `#[path]` since a test crate root
//! resolves bare `mod` names against `tests/`, not the subdir.
fn manifest_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR for tests"),
    )
}

#[path = "support/temp_dir.rs"]
mod temp_dir;

#[path = "glamour/dom.rs"]
mod dom;
#[path = "glamour/html_nul.rs"]
mod html_nul;
