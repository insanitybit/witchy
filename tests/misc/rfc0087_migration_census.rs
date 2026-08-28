//! RFC-0087 checked-in migration-census proof.

use std::process::Command;

#[test]
fn repository_census_matches_the_checked_in_type_resolved_snapshot() {
    let full_census = std::env::var_os("WITCHY_FULL_CENSUS").is_some_and(|v| v == "1")
        || std::env::var_os("WITCHY_NIGHTLY").is_some_and(|v| v == "1");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rfc0087-census"));
    cmd.arg(env!("CARGO_MANIFEST_DIR"));
    if full_census {
        cmd.arg("--full");
    }
    let output = cmd.output().expect("run RFC-0087 census");
    assert!(
        output.status.success(),
        "census failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 census output");
    let committed = include_str!("../../rfcs/0087-migration-census.tsv");
    if full_census {
        assert_eq!(stdout, committed);
    } else {
        // In representative mode, verify that every detected migration finding in the representative
        // corpus matches the committed snapshot entry for that path and line.
        for line in stdout.lines() {
            if line.is_empty() || line.starts_with("witchy-files") || line.starts_with("checked") {
                break;
            }
            assert!(
                committed.lines().any(|c| c == line),
                "representative census produced unexpected or altered finding: {line}"
            );
        }
    }
    assert_eq!(
        String::from_utf8(output.stderr).expect("UTF-8 census diagnostics"),
        ""
    );
}
