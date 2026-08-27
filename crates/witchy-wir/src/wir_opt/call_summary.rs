//! RFC-0154 CallSiteSummary and Call-Graph Analysis Model.
//!
//! Every call edge in WIR carries a structured `CallSiteSummary` that models exact target,
//! argument and result layouts, ownership envelope, capture layouts, callable contracts
//! (`ordinary`, `pure`, `once`), effect/trap properties, suspension, escape state,
//! recursion classification, known constant/range facts, and deterministic frequency hints.

use crate::wir::{Kind, WirExpr, WirFunc, WirNode, WirSeq};
use crate::wir_opt::properties::{evaluate_expr, evaluate_node, EvaluationProperties, HostEffect, Suspension};
use crate::wir_opt::range_facts::{compute_fact, RangeEnv, RangeFact};

/// Callable contract specified at definition or inferred statically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CallableContract {
    Ordinary,
    Pure,
    Once,
}

/// Ownership envelope representing argument/result forwarding and capacity tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OwnershipEnvelope {
    None,
    Scalar,
    MultiLane(u32),
    Aggregate,
    Unknown,
}

/// Escape classification for callables and capture environments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EscapeState {
    NonEscaping,
    Escapes,
    Unknown,
}

/// Recursion component classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RecursionComponent {
    None,
    SelfRecursive,
    MutualRecursive,
}

/// Known constant scalar value at a call site.
#[derive(Debug, Clone, PartialEq)]
pub enum WirConstant {
    I32(i32),
    I64(i64),
    F64(f64),
    Str(u32),
}

/// Summary descriptor for a typed call edge in WIR.
#[derive(Debug, Clone, PartialEq)]
pub struct CallSiteSummary {
    /// Exact target function name, if statically known.
    pub exact_target: Option<String>,
    /// Argument layout kinds.
    pub argument_layouts: Vec<Kind>,
    /// Return layout kind.
    pub result_layout: Option<Kind>,
    /// Ownership envelope classification.
    pub ownership_envelope: OwnershipEnvelope,
    /// Capture layouts if the callee is a closure.
    pub capture_layouts: Vec<Kind>,
    /// Callable contract (`ordinary`, `pure`, `once`).
    pub callable_contract: CallableContract,
    /// Side-effect, memory access, and trap properties.
    pub effects_and_traps: EvaluationProperties,
    /// Whether the call crosses an async/generator suspension point.
    pub suspension: bool,
    /// Whether the callable value or its environment escapes.
    pub escape_state: EscapeState,
    /// Component recursion classification.
    pub recursion_component: RecursionComponent,
    /// Known constants for arguments.
    pub known_constants: Vec<Option<WirConstant>>,
    /// Known range facts for arguments.
    pub known_ranges: Vec<Option<RangeFact>>,
    /// Structural frequency hint (loop nesting * 10 + single_use bonus).
    pub weighted_frequency_hint: u32,
}

impl CallSiteSummary {
    /// Returns true if this call site has an exact target and is safe for direct specialization.
    pub fn is_exact_and_specializable(&self) -> bool {
        self.exact_target.is_some()
            && !self.suspension
            && self.effects_and_traps.host_effect == HostEffect::None
    }

    /// Returns true if the callable contract allows pure constant folding and clone sharing.
    pub fn is_pure_callable(&self) -> bool {
        self.callable_contract == CallableContract::Pure
    }
}

/// Infer the wasm `Kind` for an expression.
pub fn infer_expr_kind(expr: &WirExpr) -> Kind {
    match expr {
        WirExpr::ConstI32(_) | WirExpr::StrPtr(_) | WirExpr::MemorySize | WirExpr::ArrayLen(_) => Kind::I32,
        WirExpr::ConstI64(_) | WirExpr::ToSlot(_, _) => Kind::I64,
        WirExpr::ConstF64(_) => Kind::F64,
        WirExpr::ConstV128(_) | WirExpr::Vector { .. } => Kind::V128,
        WirExpr::FromSlot(_, kind)
        | WirExpr::Binary { kind, .. }
        | WirExpr::Unary { kind, .. }
        | WirExpr::Convert { to: kind, .. }
        | WirExpr::Load { kind, .. }
        | WirExpr::SafeLoad { kind, .. } => *kind,
        WirExpr::Load8U { .. }
        | WirExpr::SafeLoad8U { .. }
        | WirExpr::SafeKernel(_) => Kind::I32,
        WirExpr::RefNull(kind) => *kind,
        WirExpr::RefIsNull(_) => Kind::I32,
        WirExpr::StructNew { struct_id, .. }
        | WirExpr::RefCast { struct_id, .. }
        | WirExpr::RefCastNullable { struct_id, .. } => Kind::GcRef(*struct_id),
        _ => Kind::I64,
    }
}

/// Deterministic diagnostic statistics for RFC-0154 call-graph optimizations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallOptStats {
    pub exact_edges: u64,
    pub devirtualized_edges: u64,
    pub scalarized_captures: u64,
    pub adapters_removed: u64,
    pub clones_created: u64,
    pub clones_shared: u64,
    pub inlined_direct_edges: u64,
    pub expanded_recursive_edges: u64,
    pub inline_rejects_budget: u64,
    pub inline_rejects_effect: u64,
    pub inline_rejects_suspension: u64,
    pub inline_rejects_unknown_envelope: u64,
}

impl CallOptStats {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_exact_edge(&mut self) {
        self.exact_edges += 1;
    }

    pub fn record_devirtualized_edge(&mut self) {
        self.devirtualized_edges += 1;
    }

    pub fn record_scalarized_capture(&mut self) {
        self.scalarized_captures += 1;
    }

    pub fn record_adapter_removed(&mut self) {
        self.adapters_removed += 1;
    }

    pub fn record_clone_created(&mut self) {
        self.clones_created += 1;
    }

    pub fn record_clone_shared(&mut self) {
        self.clones_shared += 1;
    }

    pub fn record_inlined_direct_edge(&mut self) {
        self.inlined_direct_edges += 1;
    }

    pub fn record_expanded_recursive_edge(&mut self) {
        self.expanded_recursive_edges += 1;
    }

    pub fn record_reject_budget(&mut self) {
        self.inline_rejects_budget += 1;
    }

    pub fn record_reject_effect(&mut self) {
        self.inline_rejects_effect += 1;
    }

    pub fn record_reject_suspension(&mut self) {
        self.inline_rejects_suspension += 1;
    }

    pub fn record_reject_unknown_envelope(&mut self) {
        self.inline_rejects_unknown_envelope += 1;
    }

    pub fn format_summary(&self) -> String {
        format!(
            "CallOptStats: exact_edges={}, devirt={}, scalar_caps={}, adapters_removed={}, \
             clones(created={}, shared={}), inlined={}, recursive_expanded={}, \
             rejects(budget={}, effect={}, susp={}, env={})",
            self.exact_edges,
            self.devirtualized_edges,
            self.scalarized_captures,
            self.adapters_removed,
            self.clones_created,
            self.clones_shared,
            self.inlined_direct_edges,
            self.expanded_recursive_edges,
            self.inline_rejects_budget,
            self.inline_rejects_effect,
            self.inline_rejects_suspension,
            self.inline_rejects_unknown_envelope,
        )
    }
}

/// Extract constant from expression if available.
pub fn extract_constant(expr: &WirExpr) -> Option<WirConstant> {
    match expr {
        WirExpr::ConstI32(i) => Some(WirConstant::I32(*i)),
        WirExpr::ConstI64(i) => Some(WirConstant::I64(*i)),
        WirExpr::ConstF64(f) => Some(WirConstant::F64(*f)),
        WirExpr::StrPtr(offset) => Some(WirConstant::Str(*offset)),
        _ => None,
    }
}

/// Analyze a `WirExpr` for a call site summary.
pub fn analyze_call_site_expr(
    func: &WirFunc,
    expr: &WirExpr,
    loop_depth: u32,
    range_env: Option<&RangeEnv>,
) -> Option<CallSiteSummary> {
    match expr {
        WirExpr::Call { func: callee, args } => {
            let is_self = callee == &func.name;
            let recursion = if is_self {
                RecursionComponent::SelfRecursive
            } else {
                RecursionComponent::None
            };
            let mut constants = Vec::with_capacity(args.len());
            let mut ranges = Vec::with_capacity(args.len());
            let mut layouts = Vec::with_capacity(args.len());

            for arg in args {
                constants.push(extract_constant(arg));
                ranges.push(range_env.map(|env| compute_fact(arg, env)));
                layouts.push(infer_expr_kind(arg));
            }

            let effects = evaluate_expr(expr);
            let suspension = effects.suspension == Suspension::Present;

            Some(CallSiteSummary {
                exact_target: Some(callee.clone()),
                argument_layouts: layouts,
                result_layout: Some(infer_expr_kind(expr)),
                ownership_envelope: OwnershipEnvelope::Scalar,
                capture_layouts: Vec::new(),
                callable_contract: CallableContract::Ordinary,
                effects_and_traps: effects,
                suspension,
                escape_state: EscapeState::NonEscaping,
                recursion_component: recursion,
                known_constants: constants,
                known_ranges: ranges,
                weighted_frequency_hint: 1 + loop_depth * 10,
            })
        }
        WirExpr::CallIndirect { signature, args, index: _ } => {
            let effects = evaluate_expr(expr);
            let suspension = effects.suspension == Suspension::Present;
            let mut constants = Vec::with_capacity(args.len());
            let mut ranges = Vec::with_capacity(args.len());
            let mut layouts = Vec::with_capacity(args.len());

            for arg in args {
                constants.push(extract_constant(arg));
                ranges.push(range_env.map(|env| compute_fact(arg, env)));
                layouts.push(infer_expr_kind(arg));
            }

            Some(CallSiteSummary {
                exact_target: None,
                argument_layouts: layouts,
                result_layout: signature.results.first().copied(),
                ownership_envelope: OwnershipEnvelope::Scalar,
                capture_layouts: Vec::new(),
                callable_contract: CallableContract::Ordinary,
                effects_and_traps: effects,
                suspension,
                escape_state: EscapeState::Unknown,
                recursion_component: RecursionComponent::None,
                known_constants: constants,
                known_ranges: ranges,
                weighted_frequency_hint: 1 + loop_depth * 10,
            })
        }
        _ => None,
    }
}

/// Analyze a `WirNode` for a call site summary.
pub fn analyze_call_site_node(
    func: &WirFunc,
    node: &WirNode,
    loop_depth: u32,
    range_env: Option<&RangeEnv>,
) -> Option<CallSiteSummary> {
    match node {
        WirNode::CallStoreMulti { func: callee, args, dests } => {
            let is_self = callee == &func.name;
            let recursion = if is_self {
                RecursionComponent::SelfRecursive
            } else {
                RecursionComponent::None
            };
            let mut constants = Vec::with_capacity(args.len());
            let mut ranges = Vec::with_capacity(args.len());
            let mut layouts = Vec::with_capacity(args.len());

            for arg in args {
                constants.push(extract_constant(arg));
                ranges.push(range_env.map(|env| compute_fact(arg, env)));
                layouts.push(infer_expr_kind(arg));
            }

            let effects = evaluate_node(node);
            let suspension = effects.suspension == Suspension::Present;

            Some(CallSiteSummary {
                exact_target: Some(callee.clone()),
                argument_layouts: layouts,
                result_layout: None,
                ownership_envelope: OwnershipEnvelope::MultiLane(dests.len() as u32),
                capture_layouts: Vec::new(),
                callable_contract: CallableContract::Ordinary,
                effects_and_traps: effects,
                suspension,
                escape_state: EscapeState::NonEscaping,
                recursion_component: recursion,
                known_constants: constants,
                known_ranges: ranges,
                weighted_frequency_hint: 1 + loop_depth * 10,
            })
        }
        WirNode::CallIndirectStoreMulti { signature: _, args, index: _, dests } => {
            let effects = evaluate_node(node);
            let suspension = effects.suspension == Suspension::Present;
            let mut constants = Vec::with_capacity(args.len());
            let mut ranges = Vec::with_capacity(args.len());
            let mut layouts = Vec::with_capacity(args.len());

            for arg in args {
                constants.push(extract_constant(arg));
                ranges.push(range_env.map(|env| compute_fact(arg, env)));
                layouts.push(infer_expr_kind(arg));
            }

            Some(CallSiteSummary {
                exact_target: None,
                argument_layouts: layouts,
                result_layout: None,
                ownership_envelope: OwnershipEnvelope::MultiLane(dests.len() as u32),
                capture_layouts: Vec::new(),
                callable_contract: CallableContract::Ordinary,
                effects_and_traps: effects,
                suspension,
                escape_state: EscapeState::Unknown,
                recursion_component: RecursionComponent::None,
                known_constants: constants,
                known_ranges: ranges,
                weighted_frequency_hint: 1 + loop_depth * 10,
            })
        }
        _ => None,
    }
}

/// Collect all call-site summaries throughout a function.
pub fn collect_call_site_summaries(func: &WirFunc) -> Vec<CallSiteSummary> {
    let mut summaries = Vec::new();
    collect_summaries_seq(func, &func.body, 0, &mut summaries);
    summaries
}

fn collect_summaries_seq(
    func: &WirFunc,
    seq: &WirSeq,
    loop_depth: u32,
    out: &mut Vec<CallSiteSummary>,
) {
    for node in seq {
        collect_summaries_node(func, node, loop_depth, out);
    }
}

fn collect_summaries_node(
    func: &WirFunc,
    node: &WirNode,
    loop_depth: u32,
    out: &mut Vec<CallSiteSummary>,
) {
    if let Some(summary) = analyze_call_site_node(func, node, loop_depth, None) {
        out.push(summary);
    }

    match node {
        WirNode::Source { body, .. } | WirNode::Block { body, .. } => {
            collect_summaries_seq(func, body, loop_depth, out);
        }
        WirNode::Loop { body, .. } => {
            collect_summaries_seq(func, body, loop_depth + 1, out);
        }
        WirNode::If { cond, then_, els, .. } => {
            collect_summaries_expr(func, cond, loop_depth, out);
            collect_summaries_seq(func, then_, loop_depth, out);
            collect_summaries_seq(func, els, loop_depth, out);
        }
        WirNode::SetLocal { value, .. }
        | WirNode::SetGlobal { value, .. }
        | WirNode::Drop(value)
        | WirNode::Do(value)
        | WirNode::Push(value)
        | WirNode::Return(Some(value))
        | WirNode::Br { cond: Some(value), .. } => {
            collect_summaries_expr(func, value, loop_depth, out);
        }
        WirNode::Store { ptr, value, .. }
        | WirNode::Store8 { ptr, value, .. }
        | WirNode::StructSet { base: ptr, value, .. } => {
            collect_summaries_expr(func, ptr, loop_depth, out);
            collect_summaries_expr(func, value, loop_depth, out);
        }
        WirNode::SafeStore { place, value, .. }
        | WirNode::SafeStore8 { place, value } => {
            place.for_each_wir_expr(|expr| collect_summaries_expr(func, expr, loop_depth, out));
            collect_summaries_expr(func, value, loop_depth, out);
        }
        WirNode::SafeKernel(operation) => {
            operation.for_each_wir_expr(|expr| {
                collect_summaries_expr(func, expr, loop_depth, out)
            });
        }
        WirNode::ArraySet { array, index, value, .. } => {
            collect_summaries_expr(func, array, loop_depth, out);
            collect_summaries_expr(func, index, loop_depth, out);
            collect_summaries_expr(func, value, loop_depth, out);
        }
        WirNode::MemoryCopy { dest, src, len } => {
            collect_summaries_expr(func, dest, loop_depth, out);
            collect_summaries_expr(func, src, loop_depth, out);
            collect_summaries_expr(func, len, loop_depth, out);
        }
        WirNode::SafeMemoryCopy { destination, source, len, .. } => {
            destination
                .for_each_wir_expr(|expr| collect_summaries_expr(func, expr, loop_depth, out));
            source.for_each_wir_expr(|expr| collect_summaries_expr(func, expr, loop_depth, out));
            collect_summaries_expr(func, len, loop_depth, out);
        }
        WirNode::MemoryFill { dest, value, len } => {
            collect_summaries_expr(func, dest, loop_depth, out);
            collect_summaries_expr(func, value, loop_depth, out);
            collect_summaries_expr(func, len, loop_depth, out);
        }
        WirNode::SafeMemoryFill { destination, value, len } => {
            destination
                .for_each_wir_expr(|expr| collect_summaries_expr(func, expr, loop_depth, out));
            collect_summaries_expr(func, value, loop_depth, out);
            collect_summaries_expr(func, len, loop_depth, out);
        }
        _ => {}
    }
}

fn collect_summaries_expr(
    func: &WirFunc,
    expr: &WirExpr,
    loop_depth: u32,
    out: &mut Vec<CallSiteSummary>,
) {
    if let Some(summary) = analyze_call_site_expr(func, expr, loop_depth, None) {
        out.push(summary);
    }

    match expr {
        WirExpr::Binary { lhs, rhs, .. } => {
            collect_summaries_expr(func, lhs, loop_depth, out);
            collect_summaries_expr(func, rhs, loop_depth, out);
        }
        WirExpr::Unary { arg, .. }
        | WirExpr::Convert { arg, .. }
        | WirExpr::ToSlot(arg, _)
        | WirExpr::FromSlot(arg, _)
        | WirExpr::Load { ptr: arg, .. }
        | WirExpr::Load8U { ptr: arg, .. }
        | WirExpr::MemoryGrow(arg)
        | WirExpr::StructGet { base: arg, .. }
        | WirExpr::ArrayLen(arg)
        | WirExpr::RefCast { value: arg, .. }
        | WirExpr::RefCastNullable { value: arg, .. }
        | WirExpr::RefIsNull(arg) => {
            collect_summaries_expr(func, arg, loop_depth, out);
        }
        WirExpr::SafeLoad { place, .. } | WirExpr::SafeLoad8U { place } => {
            place.for_each_wir_expr(|expr| collect_summaries_expr(func, expr, loop_depth, out));
        }
        WirExpr::SafeKernel(operation) => {
            operation.for_each_wir_expr(|expr| {
                collect_summaries_expr(func, expr, loop_depth, out)
            });
        }
        WirExpr::ArrayGet { array, index, .. } => {
            collect_summaries_expr(func, array, loop_depth, out);
            collect_summaries_expr(func, index, loop_depth, out);
        }
        WirExpr::ArrayNew { value, len, .. } => {
            collect_summaries_expr(func, value, loop_depth, out);
            collect_summaries_expr(func, len, loop_depth, out);
        }
        WirExpr::ArrayNewFixed { items, .. } | WirExpr::StructNew { args: items, .. } => {
            for item in items {
                collect_summaries_expr(func, item, loop_depth, out);
            }
        }
        WirExpr::Vector { args, .. } => {
            for arg in args {
                collect_summaries_expr(func, arg, loop_depth, out);
            }
        }
        WirExpr::Control(node) => {
            collect_summaries_node(func, node, loop_depth, out);
        }
        WirExpr::Seq(seq) => {
            collect_summaries_seq(func, seq, loop_depth, out);
        }
        _ => {}
    }
}
