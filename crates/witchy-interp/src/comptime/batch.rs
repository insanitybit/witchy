//! Batched execution of consecutive `comptime:` blocks.
//!
//! Every block runs as its own synthetic program: the reachable local helpers,
//! the std closure (always `meta`), and a `main` holding the block. Linking,
//! checking, and preparing that program costs ~0.28s in a debug build (check
//! 95ms, preparation 176ms, measured 2026-09-25) and is almost all std;
//! running the block is 1-2ms. A `derive(Eq, Ord, ...)` list schedules one
//! block per derive, each a program that differs from its neighbours ONLY in
//! `main`.
//!
//! So when a block starts, the expander speculates the programs of the pending
//! blocks behind it from the module as it stands, keeps the contiguous run
//! whose programs match this one except for `main`, and links, checks, and
//! prepares ONE program carrying each block's body as its own entry function.
//! Each entry then runs in a fresh interpreter with the budget, seed, and
//! identifier scope its unbatched run would have had.
//!
//! A batched result is only used for a block whose program, rebuilt from the
//! module at the moment it is reached, equals the speculated one (earlier
//! blocks' output can change `module_types`, helpers, or imports) and whose
//! identifier scope matches. Any failure — link, check, preparation, or an
//! entry's run — falls back to the unbatched path, so every diagnostic is
//! produced exactly as before; batching only ever supplies successful runs.

use std::collections::VecDeque;

use witchy_syntax::ast::{Function, Item, Module};

use super::build_comptime_program;
use crate::interpreter::{PositionedComptimeItem, RuntimeError};

pub(super) type ComptimeRun = (Vec<String>, Vec<PositionedComptimeItem>);

/// Speculation that keeps diverging is wasted preparation; stop batching the
/// module after this many restarts.
pub(super) const MAX_BATCH_RESTARTS: usize = 4;

/// Bound on the blocks prepared together (the first plus speculated ones).
const MAX_BATCH_BLOCKS: usize = 64;

const ENTRY_PREFIX: &str = "comptime_batch_entry_";

/// `WITCHY_COMPTIME_BATCH=off` runs every block alone (the A/B baseline).
pub(super) fn enabled() -> bool {
    std::env::var_os("WITCHY_COMPTIME_BATCH").is_none_or(|value| value != "off")
}

pub(super) struct ComptimeBatch {
    /// Speculated program and identifier scope of each pending block, in the
    /// order the expander will reach them, with that entry's run.
    pending: VecDeque<(Module, Option<String>, Result<ComptimeRun, RuntimeError>)>,
}

pub(super) enum BatchNext {
    Ran(ComptimeRun),
    /// The block is the speculated one but its batched run failed.
    Failed,
    /// The block's program or scope is not the speculated one.
    Diverged,
}

pub(super) enum BatchStart {
    Started(ComptimeRun, Option<ComptimeBatch>),
    /// No later block shares this block's program; run it alone.
    Nothing,
    /// The batched program failed somewhere; run blocks alone from here on.
    Failed,
}

impl ComptimeBatch {
    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub(super) fn take_next(&mut self, prog: &Module, scope: &Option<String>) -> BatchNext {
        match self.pending.pop_front() {
            Some((speculated, speculated_scope, run))
                if speculated_scope == *scope && speculated == *prog =>
            {
                match run {
                    Ok(output) => BatchNext::Ran(output),
                    Err(_) => BatchNext::Failed,
                }
            }
            _ => BatchNext::Diverged,
        }
    }

    /// `prog` is the program of the block just removed from `module.items[at]`;
    /// the pending comptime items from `at` on are the blocks the expander
    /// reaches next, numbered from `expanded + 1`.
    pub(super) fn start(
        module: &Module,
        at: usize,
        prog: &Module,
        name: &str,
        expanded: usize,
    ) -> BatchStart {
        let Some(Item::Function(main)) = prog.items.last() else {
            return BatchStart::Nothing;
        };
        if main.name != "main" || !shared_helpers_fully_annotated(prog) {
            return BatchStart::Nothing;
        }
        let mut speculated = Vec::new();
        for item in &module.items[at..] {
            if speculated.len() + 1 >= MAX_BATCH_BLOCKS {
                break;
            }
            let Item::Comptime(body) = item else { continue };
            let Ok(next) = build_comptime_program(module, body.clone()) else { break };
            if !same_program_except_main(prog, &next) {
                break;
            }
            speculated.push(next);
        }
        if speculated.is_empty() {
            return BatchStart::Nothing;
        }
        let entry_name = |k: usize| format!("{ENTRY_PREFIX}{k}");
        let taken = |candidate: &str| {
            prog.items.iter().any(|item| matches!(item, Item::Function(f) if f.name == candidate))
        };
        if (1..=speculated.len()).any(|k| taken(&entry_name(k))) {
            return BatchStart::Nothing;
        }
        let mut combined = prog.clone();
        for (k, next) in speculated.iter().enumerate() {
            let Some(Item::Function(next_main)) = next.items.last() else {
                return BatchStart::Nothing;
            };
            combined.items.push(Item::Function(Function {
                name: entry_name(k + 1),
                ..next_main.clone()
            }));
        }
        let Ok(linked) = crate::pipeline::link_internal(vec![("comptime".into(), combined)], "comptime")
        else {
            return BatchStart::Failed;
        };
        if witchy_types::typeck::check_comptime(&linked).is_err() {
            return BatchStart::Failed;
        }
        let scope = |k: usize| Some(format!("comptime:{}:{name}:{}", name.len(), expanded + k));
        let entries = std::iter::once(("main".to_string(), scope(0)))
            .chain((1..=speculated.len()).map(|k| (format!("comptime.{}", entry_name(k)), scope(k))))
            .collect();
        let Ok(runs) = crate::interpreter::run_comptime_entries_budgeted(
            linked,
            ".",
            crate::interpreter::COMPTIME_STEP_LIMIT,
            entries,
        ) else {
            return BatchStart::Failed;
        };
        let mut runs = runs.into_iter();
        let Some(Ok(first)) = runs.next() else {
            return BatchStart::Failed;
        };
        let pending: VecDeque<_> = speculated
            .into_iter()
            .zip(runs)
            .enumerate()
            .map(|(k, (next, run))| (next, scope(k + 1), run))
            .collect();
        BatchStart::Started(first, (!pending.is_empty()).then_some(ComptimeBatch { pending }))
    }
}

/// The checker gives an unannotated parameter or return a fresh type variable
/// that is not generalized, so one entry's call could pin a shared helper's
/// type that another block, checked alone, leaves unresolved and is rejected
/// for. Batch only when every local helper's signature is fully written out,
/// so no entry can change what another entry's check concludes.
fn shared_helpers_fully_annotated(prog: &Module) -> bool {
    prog.items.iter().all(|item| match item {
        Item::Function(f) if f.name == "main" => true,
        Item::Function(f) => f.ret.is_some() && f.params.iter().all(|p| p.ty.is_some()),
        _ => true,
    })
}

/// Whether two block programs differ only in their trailing `main`.
fn same_program_except_main(a: &Module, b: &Module) -> bool {
    let is_main = |m: &Module| matches!(m.items.last(), Some(Item::Function(f)) if f.name == "main");
    if !is_main(a) || !is_main(b) {
        return false;
    }
    let without_main = |m: &Module| {
        let mut m = m.clone();
        m.items.pop();
        m
    };
    without_main(a) == without_main(b)
}
