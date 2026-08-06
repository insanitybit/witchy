//! RFC-0087 checked-in migration-census proof.

use std::process::Command;

#[test]
fn repository_census_matches_the_checked_in_type_resolved_snapshot() {
    let output = Command::new(env!("CARGO_BIN_EXE_rfc0087-census"))
        .arg(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run RFC-0087 census");
    assert!(
        output.status.success(),
        "census failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8 census output"),
        include_str!("../../rfcs/0087-migration-census.tsv")
    );
    assert_eq!(
        String::from_utf8(output.stderr).expect("UTF-8 census diagnostics"),
        ""
    );
}
