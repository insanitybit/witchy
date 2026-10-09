//! Optional exact counters of allocations, retains and releases.
//!
//! `WITCHY_COUNT_MEMORY_OPS` at encode time appends three mutable `i64`
//! globals after the allocator's three and exports them for the host. Every
//! logical allocation (`emit_alloc`), every retain through the outlined
//! helper, and every `DropRef`/`Release` entering the drop worklist bumps its
//! counter. The emitter and the independent plan consult the same flag, so
//! the plan mirror stays exact and admission still compares them.
//!
//! This is a measurement aid for the kernel A/B harness
//! (`scripts/kernel-ab.py`). The counted module is slower than the ordinary
//! one, so timings are taken from uncounted runs and counts from counted ones.
use super::*;
use std::sync::OnceLock;

pub const COUNT_ALLOCS_EXPORT: &str = "__witchy_count_allocs";
pub const COUNT_RETAINS_EXPORT: &str = "__witchy_count_retains";
pub const COUNT_RELEASES_EXPORT: &str = "__witchy_count_releases";

/// The allocator owns globals 0..=2; the counters follow.
const FIRST_GLOBAL: u32 = 3;

#[derive(Clone, Copy)]
pub(super) enum Counter {
    Allocs = 0,
    Retains = 1,
    Releases = 2,
}

pub(super) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("WITCHY_COUNT_MEMORY_OPS").is_some_and(|value| !value.is_empty() && value != "0")
    })
}

/// Append the counter globals after the allocator's three; a no-op when
/// disabled. The emitter and the plan mirror call this at the same point.
pub(super) fn declare_globals(globals: &mut GlobalSection) {
    if !enabled() {
        return;
    }
    for _ in 0..3 {
        globals.global(GlobalType { val_type: ValType::I64, mutable: true, shared: false }, &ConstExpr::i64_const(0));
    }
}

/// Export the counters right after the allocator's heap-audit exports; a
/// no-op when disabled. Export order is part of the plan comparison, so both
/// sides call this at the same point.
pub(super) fn declare_exports(exports: &mut ExportSection) {
    if !enabled() {
        return;
    }
    for (offset, name) in [COUNT_ALLOCS_EXPORT, COUNT_RETAINS_EXPORT, COUNT_RELEASES_EXPORT].into_iter().enumerate() {
        exports.export(name, ExportKind::Global, FIRST_GLOBAL + offset as u32);
    }
}

pub(super) fn emit_bump(body: &mut WasmFunctionEncoder, counter: Counter) {
    if !enabled() {
        return;
    }
    use wasm_encoder::Instruction as I;
    let global = FIRST_GLOBAL + counter as u32;
    for instruction in [I::GlobalGet(global), I::I64Const(1), I::I64Add, I::GlobalSet(global)] {
        body.instruction(&instruction);
    }
}

pub(super) fn plan_bump(plan: &mut Vec<PlanToken>, counter: Counter) {
    if !enabled() {
        return;
    }
    let global = FIRST_GLOBAL + counter as u32;
    plan.extend([PlanToken::GlobalGet(global), PlanToken::I64Const(1), PlanToken::I64Add, PlanToken::GlobalSet(global)]);
}
