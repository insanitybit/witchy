//! The public `run-source` path is the only production launcher for precompiled libraries.
use std::process::{Command, Output};
use super::precompiled_library::{PrecompiledLibraryFixture, LIBRARY_ROOT};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

fn run_source(fixture: &PrecompiledLibraryFixture, owner_root: &str) -> Output {
    Command::new(BIN)
        .current_dir(&fixture.root)
        .args([
            "run-source", "src/main.witchy", "--manifest", "witchy.toml",
            "--package-owner", "workspace", "app", "1.0.0", "src.main",
            "--precompiled", "external_math=lib/math.wasm",
            "--dep-owner", "external_math", &format!("registry:{owner_root}"), "acme/math", "1.0.0", "src.math",
        ])
        .output()
        .expect("spawn witchy run-source")
}

fn refused(result: Output, expected: &str) {
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(error.contains(expected), "{error}");
    assert!(!error.contains("confinement: layer="), "must refuse before launch: {error}");
}

#[test]
fn run_source_executes_the_selected_precompiled_library() {
    let fixture = PrecompiledLibraryFixture::new();
    let result = run_source(&fixture, LIBRARY_ROOT);
    assert!(result.status.success(), "stdout={} stderr={}",
        String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
    assert_eq!(String::from_utf8_lossy(&result.stdout), "15\n");
}

/// The caller names the owner, but the export-signature bundle binds it: a
/// claim the bundle does not name, or a tampered bundle, never launches.
#[test]
fn run_source_refuses_a_relabeled_or_tampered_library_before_launch() {
    let fixture = PrecompiledLibraryFixture::new();
    refused(run_source(&fixture, "another-root"), "bundle identity does not match");
    std::fs::write(fixture.root.join("lib/math.opt"), b"tampered").unwrap();
    refused(run_source(&fixture, LIBRARY_ROOT), "invalid authenticated export-signature bundle");
}
