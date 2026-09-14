//! Offline package verification enters through its own checked source ABI.
use super::{BIN, workdir};
use std::process::Command;

#[test]
fn verify_rune_checked_entry_preserves_usage_and_missing_provenance_errors() {
    let dir = workdir("canonical-verify-rune-entry");
    let home = dir.join("home");
    let root_key = "00".repeat(32);
    for (arguments, expected) in [
        (vec!["pm", "verify-rune"],
            "usage: grimoire verify-rune <vendored-rune-dir> <registry-root-pubkey-hex>"),
        (vec!["pm", "verify-rune", "absent", root_key.as_str()],
            "no coven.json in absent"),
    ] {
        let output = Command::new(BIN).args(arguments)
            .current_dir(&dir).env("WITCHY_HOME", &home).env_remove("COVEN_URL")
            .output().expect("run checked offline verification entry");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "stdout={stdout}; stderr={stderr}");
        assert!(stdout.contains(expected), "stdout={stdout}; stderr={stderr}");
        assert!(!stderr.contains("MissingCheckedSourceWitness"), "{stderr}");
        assert!(!stderr.contains("unsupported"), "{stderr}");
        assert!(!dir.join("absent").exists(), "verification cannot create a missing rune");
    }
}
