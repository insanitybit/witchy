//! The public `run-source` path is the only production launcher for authenticated libraries.
use std::process::{Command, Output};
use super::precompiled_registry::PrecompiledRegistryFixture;

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

fn run_source(fixture: &PrecompiledRegistryFixture) -> Output {
    Command::new(BIN)
        .current_dir(&fixture.root)
        .args([
            "run-source", "src/main.witchy", "--manifest", "witchy.toml",
            "--package-owner", "workspace", "app", "1.0.0", "src.main",
        ])
        .output()
        .expect("spawn witchy run-source")
}

#[test]
fn run_source_executes_the_authenticated_precompiled_library() {
    let fixture = PrecompiledRegistryFixture::new();
    let result = run_source(&fixture);
    assert!(result.status.success(), "stdout={} stderr={}",
        String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
    assert_eq!(String::from_utf8_lossy(&result.stdout), "15\n");
}

#[test]
fn run_source_refuses_a_tampered_precompiled_library_before_launch() {
    let fixture = PrecompiledRegistryFixture::new();
    std::fs::write(
        fixture.root.join("vendor/external_math/artifacts/src/math.opt"), b"tampered",
    ).unwrap();

    let result = run_source(&fixture);
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(error.contains("precompiled registry export-signature sidecar differs from signed size"), "{error}");
    assert!(!error.contains("confinement: layer="), "must refuse before launch: {error}");
}
