//! The capability gate — **block on any widening** (§10).
//!
//! Compares a proposed resolution's footprints against the current lockfile. If
//! the proposal demands any capability kind — runtime or build — that the lock
//! did not already record, the operation is blocked until the user explicitly
//! accepts it (`--allow-cap` / `--allow-build-cap`), which records the new
//! footprint as the baseline. Narrowing or unchanged footprints are always free.

use std::collections::{BTreeMap, BTreeSet};

use super::footprint::Widening;
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

    // Which runes introduce each newly-appearing kind? Rights-aware: a rune that
    // demands `Net` (full) is a contributor to a widened `Net[Listen]`.
    let mut contributors: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for kind in agg_widening.runtime.iter().chain(agg_widening.build.iter()) {
        let mut who: Vec<String> = resolution
            .runes
            .iter()
            .filter(|r| {
                super::footprint::covers(&r.footprint.runtime, kind)
                    || super::footprint::covers(&r.footprint.build, kind)
            })
            .map(|r| r.name.clone())
            .collect();
        who.sort();
        contributors.insert(kind.clone(), who);
    }

    // What `new` demands beyond what's already locked or explicitly accepted.
    // Computed directly from the real footprints (`new − (old ∪ allowed)`), never
    // by re-differencing the rendered `agg_widening` delta — a delta string like
    // `Net[Listen]` would re-parse with its transport axis re-expanded to full,
    // which could spuriously block when a transport-narrowed grant was allowed.
    // Rights-aware: accepting `Net` (full) clears a blocked `Net[Listen]`.
    let base_runtime: BTreeSet<String> = old_agg.runtime.union(allowed_runtime).cloned().collect();
    let base_build: BTreeSet<String> = old_agg.build.union(allowed_build).cloned().collect();
    let blocking = Widening {
        runtime: super::footprint::cap_difference(&new_agg.runtime, &base_runtime),
        build: super::footprint::cap_difference(&new_agg.build, &base_build),
    };

    // Per-rune deltas vs each rune's own previous entry (upgrades).
    let mut per_rune = Vec::new();
    for r in &resolution.runes {
        let base = old_lock.footprint_of(&r.name).unwrap_or_default();
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

    #[test]
    fn gaining_a_verb_blocks_verb_precisely_and_bare_allow_clears_it() {
        // Lock has a connect-only client; the upgrade also wants to listen — a
        // verb-precise widening, blocked until accepted.
        let old = Resolution {
            runes: vec![rune("acme/http", &["Net[Connect]"])],
        }
        .to_lockfile();
        let new = Resolution {
            runes: vec![rune("acme/http", &["Net[Connect, Listen]"])],
        };
        let report = check(&new, &old, &BTreeSet::new(), &BTreeSet::new());
        assert!(report.is_blocked());
        assert!(report.blocking.runtime.contains("Net[Listen]"));
        assert_eq!(report.contributors["Net[Listen]"], vec!["acme/http".to_string()]);

        // Accepting the bare capability (`--allow-cap Net`) covers the new verb.
        let allowed: BTreeSet<String> = ["Net".to_string()].into_iter().collect();
        let report = check(&new, &old, &allowed, &BTreeSet::new());
        assert!(!report.is_blocked());
    }

    #[test]
    fn gaining_a_transport_blocks_and_bare_allow_clears_it() {
        // Lock pins the client to TCP; the upgrade opens to all transports — a
        // transport-precise widening (gains Udp/Uds), blocked until accepted.
        let old = Resolution {
            runes: vec![rune("acme/http", &["Net[Connect, Tcp]"])],
        }
        .to_lockfile();
        let new = Resolution {
            runes: vec![rune("acme/http", &["Net[Connect]"])],
        };
        let report = check(&new, &old, &BTreeSet::new(), &BTreeSet::new());
        assert!(report.is_blocked());
        assert!(
            report.blocking.runtime.iter().any(|k| k.contains("Udp")),
            "should block the gained transport: {:?}",
            report.blocking.runtime
        );
        // Accepting the bare capability clears it.
        let allowed: BTreeSet<String> = ["Net".to_string()].into_iter().collect();
        assert!(!check(&new, &old, &allowed, &BTreeSet::new()).is_blocked());

        // The reverse (pinning an open client back to TCP) is a free narrowing.
        let lock_open = Resolution {
            runes: vec![rune("acme/http", &["Net[Connect]"])],
        }
        .to_lockfile();
        let pinned = Resolution {
            runes: vec![rune("acme/http", &["Net[Connect, Tcp]"])],
        };
        assert!(!check(&pinned, &lock_open, &BTreeSet::new(), &BTreeSet::new()).is_blocked());
    }
}
