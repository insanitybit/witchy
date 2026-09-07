//! RFC corpus status hygiene.
//!
//! `rfcs/README.md` defines a status lifecycle and two mechanical obligations:
//! every RFC carries a machine-readable status, and a `superseded` RFC names its
//! successor. Neither was checked, so both drifted (a prose `Status:` line that
//! no tool can read; a `superseded-by:` left blank). These are filesystem checks
//! only — no compilation, no fixtures.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const LIFECYCLE: &[&str] = &[
    "proposed",
    "accepted",
    "planned",
    "deferred",
    "implemented",
    "rejected",
    "superseded",
];

/// Companion documents (ledgers, migration/performance reports, normative
/// judgments) sit beside an RFC and are settled by its status, not their own.
fn is_companion(name: &str) -> bool {
    name.contains("-acceptance-ledger")
        || name.contains("-migration-report")
        || name.contains("-performance-report")
        || name.contains("-judgment")
        || name.contains("-review")
        || name.contains("-rule-manifest")
        || name.contains("-memory-mode-boundary")
}

fn rfcs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("rfcs")
}

/// The YAML frontmatter block, if the file opens with one.
fn frontmatter(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    Some(&rest[..end])
}

/// A top-level scalar field's value. Continuation lines are not needed here:
/// every field we check is a single-line scalar.
fn field<'a>(fm: &'a str, key: &str) -> Option<&'a str> {
    fm.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
        .map(str::trim)
}

/// Numbered RFC bodies, excluding companion documents.
fn rfc_bodies() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in fs::read_dir(rfcs_dir()).expect("read rfcs/") {
        let name = entry.expect("dir entry").file_name().to_string_lossy().into_owned();
        let numbered = name.len() >= 5 && name[..4].chars().all(|c| c.is_ascii_digit());
        if !name.ends_with(".md") || !numbered || is_companion(&name) {
            continue;
        }
        let text = fs::read_to_string(rfcs_dir().join(&name)).expect("read rfc");
        out.push((name, text));
    }
    out.sort();
    assert!(out.len() > 100, "expected the full RFC corpus, found {}", out.len());
    out
}

#[test]
fn every_rfc_declares_a_lifecycle_status() {
    let mut bad = Vec::new();
    for (name, text) in rfc_bodies() {
        match frontmatter(&text).and_then(|fm| field(fm, "status")) {
            None => bad.push(format!("{name}: no `status:` in YAML frontmatter")),
            Some(status) => {
                // Tolerate a trailing `# proposed | accepted | ...` legend.
                let status = status.split('#').next().unwrap_or("").trim();
                if !LIFECYCLE.contains(&status) {
                    bad.push(format!("{name}: status `{status}` is not in the lifecycle"));
                }
            }
        }
    }
    assert!(bad.is_empty(), "rfcs/README.md lifecycle violations:\n  {}", bad.join("\n  "));
}

#[test]
fn superseded_rfcs_name_their_successor() {
    let mut bad = Vec::new();
    for (name, text) in rfc_bodies() {
        let Some(fm) = frontmatter(&text) else { continue };
        if field(fm, "status").map(str::trim) != Some("superseded") {
            continue;
        }
        if field(fm, "superseded-by").unwrap_or("").is_empty() {
            bad.push(name);
        }
    }
    assert!(
        bad.is_empty(),
        "superseded RFCs must set `superseded-by:`:\n  {}",
        bad.join("\n  ")
    );
}

#[test]
fn rfc_cross_links_resolve() {
    let present: BTreeSet<String> = fs::read_dir(rfcs_dir())
        .expect("read rfcs/")
        .map(|e| e.expect("dir entry").file_name().to_string_lossy().into_owned())
        .collect();

    let mut bad = Vec::new();
    for entry in fs::read_dir(rfcs_dir()).expect("read rfcs/") {
        let name = entry.expect("dir entry").file_name().to_string_lossy().into_owned();
        if !name.ends_with(".md") {
            continue;
        }
        let text = fs::read_to_string(rfcs_dir().join(&name)).expect("read rfc");
        for target in sibling_links(&text) {
            if !present.contains(&target) {
                bad.push(format!("{name} -> {target}"));
            }
        }
    }
    assert!(bad.is_empty(), "dangling links inside rfcs/:\n  {}", bad.join("\n  "));
}

/// `](<name>.md)` and `](./<name>.md)` targets — links to siblings in `rfcs/`.
/// Anything with a further path component points outside and is not our business.
fn sibling_links(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (idx, _) in text.match_indices("](") {
        let rest = &text[idx + 2..];
        let Some(close) = rest.find(')') else { continue };
        let target = rest[..close].split('#').next().unwrap_or("");
        let target = target.strip_prefix("./").unwrap_or(target);
        if target.ends_with(".md") && !target.contains('/') {
            out.push(target.to_string());
        }
    }
    out
}
