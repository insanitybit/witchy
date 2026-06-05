//! The capability gate — **block on any widening** (§10).
//!
//! Compares a proposed resolution's footprints against the current lockfile. If
//! the proposal demands any capability kind — runtime or build — that the lock
//! did not already record, the operation is blocked until the user explicitly
//! accepts it (`--allow-cap` / `--allow-build-cap`), which records the new
//! footprint as the baseline. Narrowing or unchanged footprints are always free.

use std::collections::{BTreeMap, BTreeSet};

use super::footprint::{Footprint, Widening};
use super::lockfile::Lockfile;
use super::resolve::Resolution;

#[derive(Debug, Default)]
pub struct GateReport {
    /// New capability kinds remaining after the user's `--allow-*` acceptances —
    /// if non-empty, the operation is blocked.
    pub blocking: Widening,
    /// Every new kind (before acceptances) mapped to the runes that introduce it.
    pub contributors: BTreeMap<String, Vec<String>>,
    /// Per-rune widening versus that rune's own previous lock entry (for upgrades).
    pub per_rune: Vec<(String, Widening)>,
}

impl GateReport {
    pub fn is_blocked(&self) -> bool {
        !self.blocking.is_empty()
    }
}

/// Run the gate. `allowed_runtime` / `allowed_build` are the kinds the user has
/// explicitly accepted this invocation.
pub fn check(
    resolution: &Resolution,
    old_lock: &Lockfile,
    allowed_runtime: &BTreeSet<String>,
    allowed_build: &BTreeSet<String>,
) -> GateReport {
    let old_agg = old_lock.aggregate_footprint();
    let new_agg = resolution.aggregate_footprint();
    let agg_widening = new_agg.widening_over(&old_agg);

    // Which runes introduce each newly-appearing kind?
    let mut contributors: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for kind in agg_widening.runtime.iter().chain(agg_widening.build.iter()) {
        let mut who: Vec<String> = resolution
            .runes
            .iter()
            .filter(|r| r.footprint.runtime.contains(kind) || r.footprint.build.contains(kind))
            .map(|r| r.name.clone())
            .collect();
        who.sort();
        contributors.insert(kind.clone(), who);
    }

    // Subtract the explicitly-accepted kinds to get what remains blocking.
    let blocking = Widening {
        runtime: agg_widening
            .runtime
            .difference(allowed_runtime)
            .cloned()
            .collect(),
        build: agg_widening
            .build
            .difference(allowed_build)
            .cloned()
            .collect(),
    };

    // Per-rune deltas vs each rune's own previous entry (upgrades).
    let mut per_rune = Vec::new();
    for r in &resolution.runes {
        let base = old_lock
            .footprint_of(&r.name)
            .unwrap_or_else(Footprint::default);
        let w = r.footprint.widening_over(&base);
        if !w.is_empty() {
            per_rune.push((r.name.clone(), w));
        }
    }

    GateReport {
        blocking,
        contributors,
        per_rune,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pm::footprint::Footprint;
    use crate::pm::resolve::ResolvedRune;
    use crate::pm::store::RuneSource;

    fn rune(name: &str, rt: &[&str]) -> ResolvedRune {
        ResolvedRune {
            name: name.into(),
            version: "1.0.0".into(),
            registry: Some("coven".into()),
            source_kind: None,
            hash: "sha256:x".into(),
            footprint: Footprint {
                runtime: rt.iter().map(|s| s.to_string()).collect(),
                build: BTreeSet::new(),
            },
            provenance: None,
            src: RuneSource { files: vec![] },
        }
    }

    #[test]
    fn new_cap_blocks_without_allow() {
        let res = Resolution {
            runes: vec![rune("acme/http", &["Net"])],
        };
        let lock = Lockfile::default();
        let report = check(&res, &lock, &BTreeSet::new(), &BTreeSet::new());
        assert!(report.is_blocked());
        assert!(report.blocking.runtime.contains("Net"));
        assert_eq!(report.contributors["Net"], vec!["acme/http".to_string()]);
    }

    #[test]
    fn allowing_the_cap_unblocks() {
        let res = Resolution {
            runes: vec![rune("acme/http", &["Net"])],
        };
        let lock = Lockfile::default();
        let allowed: BTreeSet<String> = ["Net".to_string()].into_iter().collect();
        let report = check(&res, &lock, &allowed, &BTreeSet::new());
        assert!(!report.is_blocked());
    }

    #[test]
    fn unchanged_footprint_is_free() {
        let res = Resolution {
            runes: vec![rune("acme/json", &[])],
        };
        let lock = res.to_lockfile();
        let report = check(&res, &lock, &BTreeSet::new(), &BTreeSet::new());
        assert!(!report.is_blocked());
    }

    #[test]
    fn upgrade_that_widens_is_caught_per_rune() {
        // Lock has acme/http with no caps; new resolution demands Net.
        let old = Resolution {
            runes: vec![rune("acme/http", &[])],
        }
        .to_lockfile();
        let new = Resolution {
            runes: vec![rune("acme/http", &["Net"])],
        };
        let report = check(&new, &old, &BTreeSet::new(), &BTreeSet::new());
        assert!(report.is_blocked());
        assert_eq!(report.per_rune.len(), 1);
        assert!(report.per_rune[0].1.runtime.contains("Net"));
    }
}
