//! Browser-host parity for the complete compiled encoding ABI (BUG-158).

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

#[test]
fn browser_encoding_abi_matches_native() {
    if !Command::new("node").arg("--version").output().is_ok_and(|out| out.status.success()) {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }

    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/encoding-abi.test.mjs");
    let output = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn encoding ABI test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "browser encoding ABI test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("ENCODING-ABI OK"), "driver did not report success:\n{stdout}");
}
