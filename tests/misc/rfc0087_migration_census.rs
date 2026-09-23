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
        let findings = stdout
            .lines()
            .take_while(|line| !line.is_empty())
            .collect::<Vec<_>>();
        for line in &findings {
            assert!(
                committed.lines().any(|committed| committed == *line),
                "representative census produced unexpected or altered finding: {line}"
            );
        }
        for prefix in ["benchmarks/", "examples/", "projects/", "std/"] {
            assert!(
                findings.iter().any(|line| line.starts_with(prefix)),
                "representative census must retain a finding from {prefix}"
            );
        }
        let categories = [
            "mechanical-self-reassignment",
            "expression-position-mutator",
            "immutable-var-argument",
            "temporary-var-argument",
            "auxiliary-result-discard",
        ];
        for category in categories {
            let summary_prefix = format!("{category}\t");
            let committed_count = committed
                .lines()
                .find_map(|line| line.strip_prefix(&summary_prefix))
                .expect("full census records every migration category")
                .parse::<usize>()
                .expect("full census category count is numeric");
            if committed_count > 0 {
                let finding_marker = format!("\t{category}\t");
                assert!(
                    findings.iter().any(|line| line.contains(&finding_marker)),
                    "representative census must retain active category {category}"
                );
            }
        }
    }
    assert_eq!(
        String::from_utf8(output.stderr).expect("UTF-8 census diagnostics"),
        ""
    );
}
