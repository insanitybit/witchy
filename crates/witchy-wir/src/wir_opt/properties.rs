//! RFC-0153 WIR evaluation properties.
//!
//! Every WIR expression and statement node exposes compositional evaluation
//! properties describing host effects, suspension, memory access, trapping
//! potential, and allocation.
//!
//! Optimizations consult these properties to decide whether an expression may
//! be safely eliminated, reordered, or replaced.

use std::collections::BTreeSet;
use crate::wir::{BinOp, WirExpr, WirNode, WirSeq};

/// Host effect classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HostEffect {
    None,
    Present,
}

/// Suspension classification (async/generator boundaries).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Suspension {
    None,
    Present,
}

/// Memory access summary for heap, global, or linear memory roots.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemoryAccess {
    None,
    Read(BTreeSet<String>),
    Write(BTreeSet<String>),
    ReadWrite(BTreeSet<String>),
    Unknown,
}

impl MemoryAccess {
    pub fn is_none(&self) -> bool {
        matches!(self, MemoryAccess::None)
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, MemoryAccess::Unknown)
    }

    pub fn reads(&self) -> Option<&BTreeSet<String>> {
        match self {
            MemoryAccess::Read(roots) | MemoryAccess::ReadWrite(roots) => Some(roots),
            _ => None,
        }
    }

    pub fn writes(&self) -> Option<&BTreeSet<String>> {
        match self {
            MemoryAccess::Write(roots) | MemoryAccess::ReadWrite(roots) => Some(roots),
            _ => None,
        }
    }

    pub fn combine(&self, other: &Self) -> Self {
        match (self, other) {
            (MemoryAccess::Unknown, _) | (_, MemoryAccess::Unknown) => MemoryAccess::Unknown,
            (MemoryAccess::None, other) => other.clone(),
            (this, MemoryAccess::None) => this.clone(),
            (MemoryAccess::Read(r1), MemoryAccess::Read(r2)) => {
                let mut set = r1.clone();
                set.extend(r2.iter().cloned());
                MemoryAccess::Read(set)
            }
            (MemoryAccess::Write(w1), MemoryAccess::Write(w2)) => {
                let mut set = w1.clone();
                set.extend(w2.iter().cloned());
                MemoryAccess::Write(set)
            }
            (MemoryAccess::Read(r), MemoryAccess::Write(w))
            | (MemoryAccess::Write(w), MemoryAccess::Read(r)) => {
                let mut set = r.clone();
                set.extend(w.iter().cloned());
                MemoryAccess::ReadWrite(set)
            }
            (MemoryAccess::ReadWrite(rw1), MemoryAccess::Read(r2))
            | (MemoryAccess::Read(r2), MemoryAccess::ReadWrite(rw1)) => {
                let mut set = rw1.clone();
                set.extend(r2.iter().cloned());
                MemoryAccess::ReadWrite(set)
            }
            (MemoryAccess::ReadWrite(rw1), MemoryAccess::Write(w2))
            | (MemoryAccess::Write(w2), MemoryAccess::ReadWrite(rw1)) => {
                let mut set = rw1.clone();
                set.extend(w2.iter().cloned());
                MemoryAccess::ReadWrite(set)
            }
            (MemoryAccess::ReadWrite(rw1), MemoryAccess::ReadWrite(rw2)) => {
                let mut set = rw1.clone();
                set.extend(rw2.iter().cloned());
                MemoryAccess::ReadWrite(set)
            }
        }
    }

    pub fn is_disjoint_from(&self, other: &Self) -> bool {
        match (self, other) {
            (MemoryAccess::None, _) | (_, MemoryAccess::None) => true,
            (MemoryAccess::Unknown, _) | (_, MemoryAccess::Unknown) => false,
            _ => {
                let w1 = self.writes();
                let r1 = self.reads();
                let w2 = other.writes();
                let r2 = other.reads();

                // If self writes, other must not read or write those roots
                if let Some(writes) = w1 {
                    if let Some(other_reads) = r2 {
                        if !writes.is_disjoint(other_reads) {
                            return false;
                        }
                    }
                    if let Some(other_writes) = w2 {
                        if !writes.is_disjoint(other_writes) {
                            return false;
                        }
                    }
                }
                // If other writes, self must not read those roots
                if let Some(other_writes) = w2 {
                    if let Some(reads) = r1 {
                        if !other_writes.is_disjoint(reads) {
                            return false;
                        }
                    }
                }
                true
            }
        }
    }
}

/// Allocation behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AllocationKind {
    None,
    Present,
    Unknown,
}

/// Compositional evaluation properties for a WIR expression or node.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EvaluationProperties {
    pub host_effect: HostEffect,
    pub suspension: Suspension,
    pub memory: MemoryAccess,
    pub may_trap: bool,
    pub allocation: AllocationKind,
    pub total: bool,
}

impl EvaluationProperties {
    /// Pure, side-effect-free, trap-free total evaluation.
    pub const fn pure_total() -> Self {
        Self {
            host_effect: HostEffect::None,
            suspension: Suspension::None,
            memory: MemoryAccess::None,
            may_trap: false,
            allocation: AllocationKind::None,
            total: true,
        }
    }

    /// Pure evaluation that may trap (e.g. division by unknown variable).
    pub const fn may_trap_only() -> Self {
        Self {
            host_effect: HostEffect::None,
            suspension: Suspension::None,
            memory: MemoryAccess::None,
            may_trap: true,
            allocation: AllocationKind::None,
            total: false,
        }
    }

    /// Recompute `total` guarantee from constituent fields.
    pub fn with_derived_total(mut self) -> Self {
        self.total = self.host_effect == HostEffect::None
            && self.suspension == Suspension::None
            && self.memory.is_none()
            && !self.may_trap
            && self.allocation == AllocationKind::None;
        self
    }

    /// Compose properties sequentially (`self` executed before `other`).
    pub fn combine(&self, other: &Self) -> Self {
        let host_effect = if self.host_effect == HostEffect::Present || other.host_effect == HostEffect::Present {
            HostEffect::Present
        } else {
            HostEffect::None
        };

        let suspension = if self.suspension == Suspension::Present || other.suspension == Suspension::Present {
            Suspension::Present
        } else {
            Suspension::None
        };

        let memory = self.memory.combine(&other.memory);

        let may_trap = self.may_trap || other.may_trap;

        let allocation = match (self.allocation, other.allocation) {
            (AllocationKind::Unknown, _) | (_, AllocationKind::Unknown) => AllocationKind::Unknown,
            (AllocationKind::Present, _) | (_, AllocationKind::Present) => AllocationKind::Present,
            (AllocationKind::None, AllocationKind::None) => AllocationKind::None,
        };

        Self {
            host_effect,
            suspension,
            memory,
            may_trap,
            allocation,
            total: false,
        }.with_derived_total()
    }

    /// Join properties across diverging control branches (e.g. `If` then vs else).
    pub fn join_branches(&self, other: &Self) -> Self {
        self.combine(other)
    }

    /// True if this expression is total and can be eliminated if its result is unused.
    pub fn is_safe_to_eliminate(&self) -> bool {
        self.total
    }

    /// True if executing `self` before `other` can be legally reordered to `other` before `self`.
    pub fn is_safe_to_reorder_with(&self, other: &Self) -> bool {
        if self.host_effect == HostEffect::Present || other.host_effect == HostEffect::Present {
            return false;
        }
        if self.suspension == Suspension::Present || other.suspension == Suspension::Present {
            return false;
        }
        // Both may trap: trap order must be preserved unless at most one may trap
        if self.may_trap && other.may_trap {
            return false;
        }
        self.memory.is_disjoint_from(&other.memory)
    }
}

/// Derive evaluation properties for any WIR expression.
fn evaluate_safe_place(place: &crate::safewir_surface::SafePlaceExpr) -> EvaluationProperties {
    let mut properties = EvaluationProperties::pure_total();
    place.for_each_wir_expr(|expr| {
        properties = properties.combine(&evaluate_expr(expr));
    });
    properties
}

fn evaluate_safe_kernel_expr(
    operation: &crate::safewir_kernel::SafeKernelExpr,
) -> EvaluationProperties {
    let mut properties = EvaluationProperties::pure_total();
    operation.for_each_wir_expr(|expr| {
        properties = properties.combine(&evaluate_expr(expr));
    });
    let allocation = match operation {
        crate::safewir_kernel::SafeKernelExpr::Allocate { .. }
        | crate::safewir_kernel::SafeKernelExpr::Reuse { .. } => AllocationKind::Present,
        crate::safewir_kernel::SafeKernelExpr::Retain { .. } => AllocationKind::None,
    };
    properties.combine(&EvaluationProperties {
        host_effect: HostEffect::None,
        suspension: Suspension::None,
        memory: MemoryAccess::Unknown,
        may_trap: true,
        allocation,
        total: false,
    })
}

fn evaluate_safe_kernel_node(
    operation: &crate::safewir_kernel::SafeKernelNode,
) -> EvaluationProperties {
    let mut properties = EvaluationProperties::pure_total();
    operation.for_each_wir_expr(|expr| {
        properties = properties.combine(&evaluate_expr(expr));
    });
    properties.combine(&EvaluationProperties {
        host_effect: HostEffect::None,
        suspension: Suspension::None,
        memory: MemoryAccess::Unknown,
        may_trap: true,
        allocation: AllocationKind::None,
        total: false,
    })
}

pub fn evaluate_expr(expr: &WirExpr) -> EvaluationProperties {
    match expr {
        WirExpr::ConstI64(_)
        | WirExpr::ConstF64(_)
        | WirExpr::ConstI32(_)
        | WirExpr::ConstV128(_)
        | WirExpr::StrPtr(_)
        | WirExpr::GetLocal(_)
        | WirExpr::RefNull(_) => EvaluationProperties::pure_total(),

        WirExpr::GetGlobal(global) => EvaluationProperties {
            host_effect: HostEffect::None,
            suspension: Suspension::None,
            memory: MemoryAccess::Read(BTreeSet::from([global.clone()])),
            may_trap: false,
            allocation: AllocationKind::None,
            total: false,
        },

        WirExpr::Vector { args, .. } => {
            let mut prop = EvaluationProperties::pure_total();
            for arg in args {
                prop = prop.combine(&evaluate_expr(arg));
            }
            prop
        }

        WirExpr::ToSlot(arg, _)
        | WirExpr::FromSlot(arg, _)
        | WirExpr::Convert { arg, .. }
        | WirExpr::RefIsNull(arg) => evaluate_expr(arg),

        WirExpr::Unary { arg, .. } => evaluate_expr(arg),

        WirExpr::Binary { op, lhs, rhs, .. } => {
            let p_lhs = evaluate_expr(lhs);
            let p_rhs = evaluate_expr(rhs);
            let mut combined = p_lhs.combine(&p_rhs);

            match op {
                BinOp::Div | BinOp::Rem => {
                    // Check if rhs is a known non-zero constant not equal to -1
                    let safe_const = match rhs.as_ref() {
                        WirExpr::ConstI64(c) if *c != 0 && *c != -1 => true,
                        WirExpr::ConstI32(c) if *c != 0 && *c != -1 => true,
                        _ => false,
                    };
                    if !safe_const {
                        combined.may_trap = true;
                        combined = combined.with_derived_total();
                    }
                }
                BinOp::DivU | BinOp::RemU => {
                    let safe_const = match rhs.as_ref() {
                        WirExpr::ConstI64(c) if *c != 0 => true,
                        WirExpr::ConstI32(c) if *c != 0 => true,
                        _ => false,
                    };
                    if !safe_const {
                        combined.may_trap = true;
                        combined = combined.with_derived_total();
                    }
                }
                _ => {}
            }
            combined
        }

        WirExpr::Load { ptr, .. } | WirExpr::Load8U { ptr, .. } => {
            let p = evaluate_expr(ptr);
            p.combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Read(BTreeSet::from(["$memory".to_string()])),
                may_trap: true,
                allocation: AllocationKind::None,
                total: false,
            })
        }
        WirExpr::SafeLoad { place, .. } | WirExpr::SafeLoad8U { place } => {
            evaluate_safe_place(place).combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Read(BTreeSet::from(["$memory".to_string()])),
                may_trap: true,
                allocation: AllocationKind::None,
                total: false,
            })
        }
        WirExpr::SafeKernel(operation) => evaluate_safe_kernel_expr(operation),

        WirExpr::MemorySize => EvaluationProperties {
            host_effect: HostEffect::None,
            suspension: Suspension::None,
            memory: MemoryAccess::Read(BTreeSet::from(["$memory".to_string()])),
            may_trap: false,
            allocation: AllocationKind::None,
            total: false,
        },

        WirExpr::MemoryGrow(arg) => {
            let p = evaluate_expr(arg);
            p.combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Write(BTreeSet::from(["$memory".to_string()])),
                may_trap: false,
                allocation: AllocationKind::Present,
                total: false,
            })
        }

        WirExpr::CallHost { args, .. } => {
            let mut prop = EvaluationProperties::pure_total();
            for arg in args {
                prop = prop.combine(&evaluate_expr(arg));
            }
            prop.combine(&EvaluationProperties {
                host_effect: HostEffect::Present,
                suspension: Suspension::None,
                memory: MemoryAccess::Unknown,
                may_trap: true,
                allocation: AllocationKind::Unknown,
                total: false,
            })
        }

        WirExpr::Call { args, .. } => {
            let mut prop = EvaluationProperties::pure_total();
            for arg in args {
                prop = prop.combine(&evaluate_expr(arg));
            }
            prop.combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Unknown,
                may_trap: true,
                allocation: AllocationKind::Unknown,
                total: false,
            })
        }

        WirExpr::CallIndirect { args, index, .. } => {
            let mut prop = EvaluationProperties::pure_total();
            for arg in args {
                prop = prop.combine(&evaluate_expr(arg));
            }
            prop = prop.combine(&evaluate_expr(index));
            prop.combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Unknown,
                may_trap: true,
                allocation: AllocationKind::Unknown,
                total: false,
            })
        }

        WirExpr::StructNew { struct_id, args } => {
            let mut prop = EvaluationProperties::pure_total();
            for arg in args {
                prop = prop.combine(&evaluate_expr(arg));
            }
            prop.combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Write(BTreeSet::from([format!("$struct_{struct_id}")])),
                may_trap: false,
                allocation: AllocationKind::Present,
                total: false,
            })
        }

        WirExpr::StructGet { struct_id, base, .. } => {
            let p = evaluate_expr(base);
            p.combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Read(BTreeSet::from([format!("$struct_{struct_id}")])),
                may_trap: true,
                allocation: AllocationKind::None,
                total: false,
            })
        }

        WirExpr::ArrayNew { array_id, value, len } => {
            let p1 = evaluate_expr(value);
            let p2 = evaluate_expr(len);
            p1.combine(&p2).combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Write(BTreeSet::from([format!("$array_{array_id}")])),
                may_trap: false,
                allocation: AllocationKind::Present,
                total: false,
            })
        }

        WirExpr::ArrayNewFixed { array_id, items } => {
            let mut prop = EvaluationProperties::pure_total();
            for item in items {
                prop = prop.combine(&evaluate_expr(item));
            }
            prop.combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Write(BTreeSet::from([format!("$array_{array_id}")])),
                may_trap: false,
                allocation: AllocationKind::Present,
                total: false,
            })
        }

        WirExpr::ArrayGet { array_id, array, index } => {
            let p1 = evaluate_expr(array);
            let p2 = evaluate_expr(index);
            p1.combine(&p2).combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Read(BTreeSet::from([format!("$array_{array_id}")])),
                may_trap: true,
                allocation: AllocationKind::None,
                total: false,
            })
        }

        WirExpr::ArrayLen(array) => {
            let p = evaluate_expr(array);
            p.combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Read(BTreeSet::from(["$array_len".to_string()])),
                may_trap: true,
                allocation: AllocationKind::None,
                total: false,
            })
        }

        WirExpr::RefCast { struct_id, value } | WirExpr::RefCastNullable { struct_id, value } => {
            let p = evaluate_expr(value);
            p.combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Read(BTreeSet::from([format!("$struct_{struct_id}")])),
                may_trap: true,
                allocation: AllocationKind::None,
                total: false,
            })
        }

        WirExpr::Control(node) => evaluate_node(node),
        WirExpr::Seq(seq) => evaluate_seq(seq),
    }
}

/// Derive evaluation properties for a WIR statement node.
pub fn evaluate_node(node: &WirNode) -> EvaluationProperties {
    match node {
        WirNode::Source { body, .. } => evaluate_seq(body),

        WirNode::SetLocal { value, .. } => evaluate_expr(value),

        WirNode::SetGlobal { global, value } => {
            let p = evaluate_expr(value);
            p.combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Write(BTreeSet::from([global.clone()])),
                may_trap: false,
                allocation: AllocationKind::None,
                total: false,
            })
        }

        WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. } => {
            let p1 = evaluate_expr(ptr);
            let p2 = evaluate_expr(value);
            p1.combine(&p2).combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Write(BTreeSet::from(["$memory".to_string()])),
                may_trap: true,
                allocation: AllocationKind::None,
                total: false,
            })
        }
        WirNode::SafeStore { place, value, .. }
        | WirNode::SafeStore8 { place, value } => evaluate_safe_place(place)
            .combine(&evaluate_expr(value))
            .combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Write(BTreeSet::from(["$memory".to_string()])),
                may_trap: true,
                allocation: AllocationKind::None,
                total: false,
            }),
        WirNode::SafeKernel(operation) => evaluate_safe_kernel_node(operation),

        WirNode::CallStoreMulti { args, .. } => {
            let mut prop = EvaluationProperties::pure_total();
            for arg in args {
                prop = prop.combine(&evaluate_expr(arg));
            }
            prop.combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Unknown,
                may_trap: true,
                allocation: AllocationKind::Unknown,
                total: false,
            })
        }

        WirNode::CallIndirectStoreMulti { args, index, .. } => {
            let mut prop = EvaluationProperties::pure_total();
            for arg in args {
                prop = prop.combine(&evaluate_expr(arg));
            }
            prop = prop.combine(&evaluate_expr(index));
            prop.combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Unknown,
                may_trap: true,
                allocation: AllocationKind::Unknown,
                total: false,
            })
        }

        WirNode::MemoryCopy { dest, src, len } => {
            let p1 = evaluate_expr(dest);
            let p2 = evaluate_expr(src);
            let p3 = evaluate_expr(len);
            p1.combine(&p2).combine(&p3).combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Write(BTreeSet::from(["$memory".to_string()])),
                may_trap: true,
                allocation: AllocationKind::None,
                total: false,
            })
        }
        WirNode::SafeMemoryCopy { destination, source, len, .. } => evaluate_safe_place(destination)
            .combine(&evaluate_safe_place(source))
            .combine(&evaluate_expr(len))
            .combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Write(BTreeSet::from(["$memory".to_string()])),
                may_trap: true,
                allocation: AllocationKind::None,
                total: false,
            }),

        WirNode::MemoryFill { dest, value, len } => {
            let p1 = evaluate_expr(dest);
            let p2 = evaluate_expr(value);
            let p3 = evaluate_expr(len);
            p1.combine(&p2).combine(&p3).combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Write(BTreeSet::from(["$memory".to_string()])),
                may_trap: true,
                allocation: AllocationKind::None,
                total: false,
            })
        }
        WirNode::SafeMemoryFill { destination, value, len } => evaluate_safe_place(destination)
            .combine(&evaluate_expr(value))
            .combine(&evaluate_expr(len))
            .combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Write(BTreeSet::from(["$memory".to_string()])),
                may_trap: true,
                allocation: AllocationKind::None,
                total: false,
            }),

        WirNode::If { cond, then_, els, .. } => {
            let p_cond = evaluate_expr(cond);
            let p_then = evaluate_seq(then_);
            let p_else = evaluate_seq(els);
            p_cond.combine(&p_then.join_branches(&p_else))
        }

        WirNode::Block { body, .. } | WirNode::Loop { body, .. } => evaluate_seq(body),

        WirNode::Br { cond, .. } => {
            let mut prop = match cond {
                Some(c) => evaluate_expr(c),
                None => EvaluationProperties::pure_total(),
            };
            prop.total = false;
            prop
        }

        WirNode::Drop(expr) | WirNode::Do(expr) | WirNode::Push(expr) => evaluate_expr(expr),

        WirNode::Return(expr_opt) => {
            let mut prop = match expr_opt {
                Some(e) => evaluate_expr(e),
                None => EvaluationProperties::pure_total(),
            };
            prop.total = false;
            prop
        }

        WirNode::Unreachable => EvaluationProperties {
            host_effect: HostEffect::None,
            suspension: Suspension::None,
            memory: MemoryAccess::None,
            may_trap: true,
            allocation: AllocationKind::None,
            total: false,
        },

        WirNode::StructSet { struct_id, base, value, .. } => {
            let p1 = evaluate_expr(base);
            let p2 = evaluate_expr(value);
            p1.combine(&p2).combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Write(BTreeSet::from([format!("$struct_{struct_id}")])),
                may_trap: true,
                allocation: AllocationKind::None,
                total: false,
            })
        }

        WirNode::ArraySet { array_id, array, index, value } => {
            let p1 = evaluate_expr(array);
            let p2 = evaluate_expr(index);
            let p3 = evaluate_expr(value);
            p1.combine(&p2).combine(&p3).combine(&EvaluationProperties {
                host_effect: HostEffect::None,
                suspension: Suspension::None,
                memory: MemoryAccess::Write(BTreeSet::from([format!("$array_{array_id}")])),
                may_trap: true,
                allocation: AllocationKind::None,
                total: false,
            })
        }
    }
}

/// Derive evaluation properties for a sequence of WIR nodes.
pub fn evaluate_seq(seq: &WirSeq) -> EvaluationProperties {
    let mut prop = EvaluationProperties::pure_total();
    for node in seq.iter() {
        prop = prop.combine(&evaluate_node(node));
    }
    prop
}
