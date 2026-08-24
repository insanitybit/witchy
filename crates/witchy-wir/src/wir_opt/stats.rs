//! Compiler controls and diagnostics for RFC-0153 range and loop optimization.
//!
//! A deterministic stats payload records fact lattice events, loop acceptances/rejections,
//! bounds check eliminations, header hoistings, cursorized accesses, arithmetic rewrites,
//! and effect/trap/overflow/alias/escape declines.
//!
//! Production Wasm contains no stats globals or placeholder instructions when stats are disabled.

use std::collections::BTreeMap;

/// Reason a loop candidate was rejected from canonical induction optimization.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LoopRejectionReason {
    NonCanonicalStep,
    UnstableRoot,
    PotentialEscape,
    UnprovedNonNegative,
    EffectDecline,
    TrapDecline,
    OverflowDecline,
    AliasDecline,
    EscapeDecline,
}

impl LoopRejectionReason {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::NonCanonicalStep => "non_canonical_step",
            Self::UnstableRoot => "unstable_root",
            Self::PotentialEscape => "potential_escape",
            Self::UnprovedNonNegative => "unproved_non_negative",
            Self::EffectDecline => "effect_decline",
            Self::TrapDecline => "trap_decline",
            Self::OverflowDecline => "overflow_decline",
            Self::AliasDecline => "alias_decline",
            Self::EscapeDecline => "escape_decline",
        }
    }
}

/// Deterministic diagnostic statistics for RFC-0153 range & loop optimizations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RangeOptStats {
    pub facts_created: u64,
    pub facts_joined: u64,
    pub facts_widened: u64,
    pub facts_dropped: u64,
    pub loops_accepted: u64,
    pub loop_rejections: BTreeMap<LoopRejectionReason, u64>,
    pub bounds_checks_removed: u64,
    pub bounds_checks_retained: u64,
    pub headers_hoisted: u64,
    pub cursorized_accesses: u64,
    pub arithmetic_rewrites: u64,
    pub effect_declines: u64,
    pub trap_declines: u64,
    pub overflow_declines: u64,
    pub alias_declines: u64,
    pub escape_declines: u64,
}

impl RangeOptStats {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_fact_created(&mut self) {
        self.facts_created += 1;
    }

    pub fn record_fact_joined(&mut self) {
        self.facts_joined += 1;
    }

    pub fn record_fact_widened(&mut self) {
        self.facts_widened += 1;
    }

    pub fn record_fact_dropped(&mut self) {
        self.facts_dropped += 1;
    }

    pub fn record_loop_accepted(&mut self) {
        self.loops_accepted += 1;
    }

    pub fn record_loop_rejected(&mut self, reason: LoopRejectionReason) {
        *self.loop_rejections.entry(reason).or_insert(0) += 1;
        match reason {
            LoopRejectionReason::EffectDecline => self.effect_declines += 1,
            LoopRejectionReason::TrapDecline => self.trap_declines += 1,
            LoopRejectionReason::OverflowDecline => self.overflow_declines += 1,
            LoopRejectionReason::AliasDecline => self.alias_declines += 1,
            LoopRejectionReason::EscapeDecline | LoopRejectionReason::PotentialEscape => {
                self.escape_declines += 1;
            }
            _ => {}
        }
    }

    pub fn record_bounds_check_removed(&mut self) {
        self.bounds_checks_removed += 1;
    }

    pub fn record_bounds_check_retained(&mut self) {
        self.bounds_checks_retained += 1;
    }

    pub fn record_header_hoisted(&mut self) {
        self.headers_hoisted += 1;
    }

    pub fn record_cursorized_access(&mut self) {
        self.cursorized_accesses += 1;
    }

    pub fn record_arithmetic_rewrite(&mut self) {
        self.arithmetic_rewrites += 1;
    }

    /// Produce a deterministic summary string for logs and evidence.
    pub fn format_summary(&self) -> String {
        let out = format!(
            "RangeOptStats: facts(created={}, joined={}, widened={}, dropped={}), \
             loops(accepted={}, rejected={:?}), \
             bounds_checks(removed={}, retained={}), \
             headers_hoisted={}, cursorized={}, arithmetic_rewrites={}, \
             declines(effect={}, trap={}, overflow={}, alias={}, escape={})",
            self.facts_created,
            self.facts_joined,
            self.facts_widened,
            self.facts_dropped,
            self.loops_accepted,
            self.loop_rejections,
            self.bounds_checks_removed,
            self.bounds_checks_retained,
            self.headers_hoisted,
            self.cursorized_accesses,
            self.arithmetic_rewrites,
            self.effect_declines,
            self.trap_declines,
            self.overflow_declines,
            self.alias_declines,
            self.escape_declines,
        );
        out
    }
}
