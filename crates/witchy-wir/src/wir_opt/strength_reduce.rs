//! Range-driven integer simplification (RFC-0146 Track 4).
//!
//! Uses dominating non-negative proofs and range facts attached to WIR locals to:
//! 1. Replace signed-safe remainder sequences with bitwise AND masks (`x & (2^k - 1)`).
//! 2. Replace bias-plus-shift division with direct shifts (`x >> k`).
//! 3. Fuse `(x % 2^k) == 0` into masked tests `(x & (2^k - 1)) == 0`.
//! 4. Simplify constant and identity arithmetic (`x + 0`, `x - 0`, `0 & mask`, `0 >> k`).

use std::collections::HashMap;
use crate::wir::{BinOp, Kind, UnOp, WirExpr, WirFunc, WirModule, WirNode, WirSeq};

/// Small integer range lattice for local variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeFact {
    /// No range information available.
    Unknown,
    /// Known exact constant value.
    Constant(i64),
    /// Known to be strictly positive (x >= 1).
    Positive,
    /// Known to be non-negative (x >= 0).
    NonNegative,
    /// Known to lie in the closed interval [lo, hi] (inclusive).
    ClosedRange(i64, i64),
}

impl RangeFact {
    /// Returns true if this value is provably >= 0.
    pub fn is_non_negative(&self) -> bool {
        match self {
            RangeFact::Constant(c) => *c >= 0,
            RangeFact::Positive | RangeFact::NonNegative => true,
            RangeFact::ClosedRange(lo, _) => *lo >= 0,
            RangeFact::Unknown => false,
        }
    }

    /// Returns true if this value is provably > 0.
    pub fn is_positive(&self) -> bool {
        match self {
            RangeFact::Constant(c) => *c > 0,
            RangeFact::Positive => true,
            RangeFact::NonNegative => false,
            RangeFact::ClosedRange(lo, _) => *lo > 0,
            RangeFact::Unknown => false,
        }
    }

    /// Returns the exact constant value if known.
    pub fn as_constant(&self) -> Option<i64> {
        match self {
            RangeFact::Constant(c) => Some(*c),
            RangeFact::ClosedRange(lo, hi) if lo == hi => Some(*lo),
            _ => None,
        }
    }

    pub fn lo_bound(&self) -> Option<i64> {
        match self {
            RangeFact::Constant(c) => Some(*c),
            RangeFact::Positive => Some(1),
            RangeFact::NonNegative => Some(0),
            RangeFact::ClosedRange(lo, _) => Some(*lo),
            RangeFact::Unknown => None,
        }
    }

    pub fn hi_bound(&self) -> Option<i64> {
        match self {
            RangeFact::Constant(c) => Some(*c),
            RangeFact::Positive | RangeFact::NonNegative => Some(i64::MAX),
            RangeFact::ClosedRange(_, hi) => Some(*hi),
            RangeFact::Unknown => None,
        }
    }

    pub fn from_bounds(lo: i64, hi: i64) -> Self {
        if lo > hi {
            RangeFact::Unknown
        } else if lo == hi {
            RangeFact::Constant(lo)
        } else if lo == 0 && hi == i64::MAX {
            RangeFact::NonNegative
        } else if lo == 1 && hi == i64::MAX {
            RangeFact::Positive
        } else {
            RangeFact::ClosedRange(lo, hi)
        }
    }

    /// Combine facts along the same control flow path.
    pub fn meet(&self, other: &RangeFact) -> Self {
        if *self == RangeFact::Unknown {
            return other.clone();
        }
        if *other == RangeFact::Unknown {
            return self.clone();
        }
        let lo = match (self.lo_bound(), other.lo_bound()) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        };
        let hi = match (self.hi_bound(), other.hi_bound()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        };
        match (lo, hi) {
            (Some(l), Some(h)) => {
                if l > h {
                    RangeFact::Unknown
                } else {
                    RangeFact::from_bounds(l, h)
                }
            }
            (Some(l), None) => {
                if l > 0 {
                    RangeFact::Positive
                } else if l == 0 {
                    RangeFact::NonNegative
                } else {
                    RangeFact::ClosedRange(l, i64::MAX)
                }
            }
            (None, Some(h)) => RangeFact::ClosedRange(i64::MIN, h),
            (None, None) => RangeFact::Unknown,
        }
    }

    /// Merge facts across divergent control flow join points.
    pub fn join(&self, other: &RangeFact) -> Self {
        if *self == RangeFact::Unknown || *other == RangeFact::Unknown {
            return RangeFact::Unknown;
        }
        let lo = match (self.lo_bound(), other.lo_bound()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            _ => None,
        };
        let hi = match (self.hi_bound(), other.hi_bound()) {
            (Some(a), Some(b)) => Some(a.max(b)),
            _ => None,
        };
        match (lo, hi) {
            (Some(l), Some(h)) => RangeFact::from_bounds(l, h),
            (Some(l), None) => {
                if l >= 1 {
                    RangeFact::Positive
                } else if l >= 0 {
                    RangeFact::NonNegative
                } else {
                    RangeFact::Unknown
                }
            }
            _ => RangeFact::Unknown,
        }
    }
}

/// Simplify integer operations in all functions of `module` using dominating range facts.
pub fn simplify_integer_ranges(module: &mut WirModule) {
    for func in &mut module.funcs {
        if func.raw_body.is_some() {
            continue;
        }
        simplify_func(func);
    }
}

fn simplify_func(func: &mut WirFunc) {
    let mut env = HashMap::new();
    // Parameters have unknown range by default
    for param in &func.params {
        env.insert(param.name.clone(), RangeFact::Unknown);
    }
    simplify_seq(&mut func.body, &mut env);
}

fn simplify_seq(seq: &mut WirSeq, env: &mut HashMap<String, RangeFact>) {
    let mut i = 0;
    while i < seq.len() {
        let node = &mut seq[i];
        simplify_node(node, env);

        // Update environment based on the statement effect
        match node {
            WirNode::SetLocal { local, value } => {
                let fact = compute_fact(value, env);
                env.insert(local.clone(), fact);
            }
            WirNode::Br { cond: Some(cond), .. } => {
                // If branch was NOT taken, the condition was false.
                let false_facts = extract_facts(cond, false, env);
                for (v, f) in false_facts {
                    let current = env.get(&v).cloned().unwrap_or(RangeFact::Unknown);
                    env.insert(v, current.meet(&f));
                }
            }
            _ => {}
        }
        i += 1;
    }
}

fn simplify_node(node: &mut WirNode, env: &mut HashMap<String, RangeFact>) {
    match node {
        WirNode::Source { body, .. } => simplify_seq(body, env),
        WirNode::SetLocal { value, .. } | WirNode::SetGlobal { value, .. } => {
            simplify_expr(value, env);
        }
        WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. } => {
            simplify_expr(ptr, env);
            simplify_expr(value, env);
        }
        WirNode::CallStoreMulti { args, .. } => {
            for a in args.iter_mut() {
                simplify_expr(a, env);
            }
        }
        WirNode::CallIndirectStoreMulti { args, index, .. } => {
            for a in args.iter_mut() {
                simplify_expr(a, env);
            }
            simplify_expr(index, env);
        }
        WirNode::MemoryCopy { dest, src, len } => {
            simplify_expr(dest, env);
            simplify_expr(src, env);
            simplify_expr(len, env);
        }
        WirNode::MemoryFill { dest, value, len } => {
            simplify_expr(dest, env);
            simplify_expr(value, env);
            simplify_expr(len, env);
        }
        WirNode::If { cond, then_, els, .. } => {
            simplify_expr(cond, env);

            let true_facts = extract_facts(cond, true, env);
            let false_facts = extract_facts(cond, false, env);

            let mut then_env = env.clone();
            for (v, f) in true_facts {
                let cur = then_env.get(&v).cloned().unwrap_or(RangeFact::Unknown);
                then_env.insert(v, cur.meet(&f));
            }
            simplify_seq(then_, &mut then_env);

            let mut els_env = env.clone();
            for (v, f) in false_facts {
                let cur = els_env.get(&v).cloned().unwrap_or(RangeFact::Unknown);
                els_env.insert(v, cur.meet(&f));
            }
            simplify_seq(els, &mut els_env);

            // Merge environments after the if-else
            let all_keys: Vec<String> = env.keys().cloned().collect();
            for k in all_keys {
                let t_fact = then_env.get(&k).cloned().unwrap_or(RangeFact::Unknown);
                let e_fact = els_env.get(&k).cloned().unwrap_or(RangeFact::Unknown);
                env.insert(k, t_fact.join(&e_fact));
            }
        }
        WirNode::Block { body, .. } => {
            let mut block_env = env.clone();
            simplify_seq(body, &mut block_env);
        }
        WirNode::Loop { body, .. } => {
            let mut mutated = std::collections::HashSet::new();
            find_mutated_locals(body, &mut mutated);

            let mut loop_env = env.clone();
            for v in mutated {
                if let Some(fact) = loop_env.get(&v) {
                    let widened = match fact {
                        RangeFact::Constant(c) if *c > 0 => RangeFact::Positive,
                        RangeFact::Constant(c) if *c == 0 => RangeFact::NonNegative,
                        RangeFact::Positive => RangeFact::Positive,
                        RangeFact::NonNegative => RangeFact::NonNegative,
                        RangeFact::ClosedRange(lo, _) if *lo > 0 => RangeFact::Positive,
                        RangeFact::ClosedRange(lo, _) if *lo >= 0 => RangeFact::NonNegative,
                        _ => RangeFact::Unknown,
                    };
                    loop_env.insert(v, widened);
                }
            }
            simplify_seq(body, &mut loop_env);
        }
        WirNode::StructSet { base, value, .. } => {
            simplify_expr(base, env);
            simplify_expr(value, env);
        }
        WirNode::ArraySet { array, index, value, .. } => {
            simplify_expr(array, env);
            simplify_expr(index, env);
            simplify_expr(value, env);
        }
        WirNode::Br { cond: Some(c), .. } => simplify_expr(c, env),
        WirNode::Drop(e) | WirNode::Do(e) | WirNode::Push(e) | WirNode::Return(Some(e)) => {
            simplify_expr(e, env);
        }
        WirNode::Br { cond: None, .. } | WirNode::Return(None) | WirNode::Unreachable => {}
    }
}

fn simplify_expr(expr: &mut WirExpr, env: &HashMap<String, RangeFact>) {
    // 1. Recurse bottom-up into subexpressions
    match expr {
        WirExpr::ToSlot(inner, _)
        | WirExpr::FromSlot(inner, _)
        | WirExpr::Unary { arg: inner, .. }
        | WirExpr::Convert { arg: inner, .. }
        | WirExpr::Load { ptr: inner, .. }
        | WirExpr::Load8U { ptr: inner, .. }
        | WirExpr::ArrayLen(inner)
        | WirExpr::RefIsNull(inner)
        | WirExpr::RefCast { value: inner, .. }
        | WirExpr::RefCastNullable { value: inner, .. } => simplify_expr(inner, env),
        WirExpr::Binary { lhs, rhs, .. } => {
            simplify_expr(lhs, env);
            simplify_expr(rhs, env);
        }
        WirExpr::Call { args, .. } | WirExpr::CallHost { args, .. } => {
            for a in args.iter_mut() {
                simplify_expr(a, env);
            }
        }
        WirExpr::CallIndirect { args, index, .. } => {
            for a in args.iter_mut() {
                simplify_expr(a, env);
            }
            simplify_expr(index, env);
        }
        WirExpr::MemoryGrow(pages) => simplify_expr(pages, env),
        WirExpr::Control(node) => {
            let mut node_env = env.clone();
            simplify_node(node, &mut node_env);
        }
        WirExpr::Seq(nodes) => {
            let mut seq_env = env.clone();
            simplify_seq(nodes, &mut seq_env);
        }
        WirExpr::StructNew { args, .. } | WirExpr::ArrayNewFixed { items: args, .. } => {
            for a in args.iter_mut() {
                simplify_expr(a, env);
            }
        }
        WirExpr::ArrayNew { value, len, .. } => {
            simplify_expr(value, env);
            simplify_expr(len, env);
        }
        WirExpr::ArrayGet { array, index, .. } => {
            simplify_expr(array, env);
            simplify_expr(index, env);
        }
        WirExpr::StructGet { base, .. } => simplify_expr(base, env),
        WirExpr::Vector { args, .. } => {
            for a in args.iter_mut() {
                simplify_expr(a, env);
            }
        }
        WirExpr::ConstI64(_)
        | WirExpr::ConstF64(_)
        | WirExpr::ConstI32(_)
        | WirExpr::ConstV128(_)
        | WirExpr::StrPtr(_)
        | WirExpr::MemorySize
        | WirExpr::GetLocal(_)
        | WirExpr::GetGlobal(_)
        | WirExpr::RefNull(_) => {}
    }

    // 2. Apply range-driven simplifications and strength reductions
    apply_strength_reductions(expr, env);
}

fn apply_strength_reductions(expr: &mut WirExpr, env: &HashMap<String, RangeFact>) {
    match expr {
        // (x % 2^k) == 0  or  0 == (x % 2^k) -> (x & (2^k - 1)) == 0
        WirExpr::Binary {
            op: op @ (BinOp::Eq | BinOp::Ne),
            kind,
            lhs,
            rhs,
        } => {
            if let WirExpr::ConstI64(0) = rhs.as_ref() {
                if let WirExpr::Binary {
                    op: BinOp::Rem,
                    kind: rem_kind,
                    lhs: inner_lhs,
                    rhs: inner_rhs,
                } = lhs.as_ref()
                {
                    if let Some(n) = compute_fact(inner_rhs, env).as_constant() {
                        if n > 0 && (n & (n - 1)) == 0 {
                            let mask = n - 1;
                            *expr = WirExpr::Binary {
                                op: *op,
                                kind: *kind,
                                lhs: Box::new(WirExpr::Binary {
                                    op: BinOp::And,
                                    kind: *rem_kind,
                                    lhs: inner_lhs.clone(),
                                    rhs: Box::new(WirExpr::ConstI64(mask)),
                                }),
                                rhs: Box::new(WirExpr::ConstI64(0)),
                            };
                        }
                    }
                }
            } else if let WirExpr::ConstI64(0) = lhs.as_ref() {
                if let WirExpr::Binary {
                    op: BinOp::Rem,
                    kind: rem_kind,
                    lhs: inner_lhs,
                    rhs: inner_rhs,
                } = rhs.as_ref()
                {
                    if let Some(n) = compute_fact(inner_rhs, env).as_constant() {
                        if n > 0 && (n & (n - 1)) == 0 {
                            let mask = n - 1;
                            *expr = WirExpr::Binary {
                                op: *op,
                                kind: *kind,
                                lhs: Box::new(WirExpr::ConstI64(0)),
                                rhs: Box::new(WirExpr::Binary {
                                    op: BinOp::And,
                                    kind: *rem_kind,
                                    lhs: inner_lhs.clone(),
                                    rhs: Box::new(WirExpr::ConstI64(mask)),
                                }),
                            };
                        }
                    }
                }
            }
        }

        // Remainder by power of 2 when non-negative: x % 2^k -> x & (2^k - 1)
        WirExpr::Binary {
            op: BinOp::Rem,
            kind: Kind::I64,
            lhs,
            rhs,
        } => {
            if let Some(n) = compute_fact(rhs, env).as_constant() {
                if n > 0 && (n & (n - 1)) == 0 {
                    if compute_fact(lhs, env).is_non_negative() {
                        let mask = n - 1;
                        *expr = WirExpr::Binary {
                            op: BinOp::And,
                            kind: Kind::I64,
                            lhs: lhs.clone(),
                            rhs: Box::new(WirExpr::ConstI64(mask)),
                        };
                    }
                }
            }
        }

        // Division by power of 2 when non-negative: x / 2^k -> x >> k
        WirExpr::Binary {
            op: BinOp::Div,
            kind: Kind::I64,
            lhs,
            rhs,
        } => {
            if let Some(n) = compute_fact(rhs, env).as_constant() {
                if n > 0 && (n & (n - 1)) == 0 {
                    if compute_fact(lhs, env).is_non_negative() {
                        let shift = n.trailing_zeros() as i64;
                        *expr = WirExpr::Binary {
                            op: BinOp::Shr,
                            kind: Kind::I64,
                            lhs: lhs.clone(),
                            rhs: Box::new(WirExpr::ConstI64(shift)),
                        };
                    }
                }
            }
        }

        // Sign bit extraction: (x >> 63) where x >= 0 simplifies to 0
        WirExpr::Binary {
            op: BinOp::Shr,
            kind: Kind::I64,
            lhs,
            rhs,
        } => {
            if let WirExpr::ConstI64(63) = rhs.as_ref() {
                if compute_fact(lhs, env).is_non_negative() {
                    *expr = WirExpr::ConstI64(0);
                }
            } else if let WirExpr::ConstI64(0) = lhs.as_ref() {
                *expr = WirExpr::ConstI64(0);
            }
        }

        // Arithmetic identity simplifications on constants
        WirExpr::Binary { op: BinOp::Add, kind: Kind::I64, lhs, rhs } => {
            if let WirExpr::ConstI64(0) = rhs.as_ref() {
                *expr = *lhs.clone();
            } else if let WirExpr::ConstI64(0) = lhs.as_ref() {
                *expr = *rhs.clone();
            }
        }
        WirExpr::Binary { op: BinOp::Sub, kind: Kind::I64, lhs, rhs } => {
            if let WirExpr::ConstI64(0) = rhs.as_ref() {
                *expr = *lhs.clone();
            }
        }
        WirExpr::Binary { op: BinOp::And, kind: Kind::I64, lhs, rhs } => {
            if let WirExpr::ConstI64(0) = rhs.as_ref() {
                *expr = WirExpr::ConstI64(0);
            } else if let WirExpr::ConstI64(0) = lhs.as_ref() {
                *expr = WirExpr::ConstI64(0);
            }
        }
        WirExpr::Binary { op: BinOp::Mul, kind: Kind::I64, lhs, rhs } => {
            if let WirExpr::ConstI64(1) = rhs.as_ref() {
                *expr = *lhs.clone();
            } else if let WirExpr::ConstI64(1) = lhs.as_ref() {
                *expr = *rhs.clone();
            } else if let WirExpr::ConstI64(0) = rhs.as_ref() {
                *expr = WirExpr::ConstI64(0);
            } else if let WirExpr::ConstI64(0) = lhs.as_ref() {
                *expr = WirExpr::ConstI64(0);
            }
        }
        _ => {}
    }
}

/// Compute the range fact for an expression given variable environment.
fn compute_fact(expr: &WirExpr, env: &HashMap<String, RangeFact>) -> RangeFact {
    match expr {
        WirExpr::ConstI64(n) => RangeFact::Constant(*n),
        WirExpr::ConstI32(n) => RangeFact::Constant(*n as i64),
        WirExpr::GetLocal(name) => env.get(name).cloned().unwrap_or(RangeFact::Unknown),
        WirExpr::Binary { op, kind: Kind::I64, lhs, rhs } => {
            let lf = compute_fact(lhs, env);
            let rf = compute_fact(rhs, env);
            match op {
                BinOp::Add => match (lf.as_constant(), rf.as_constant()) {
                    (Some(a), Some(b)) => a.checked_add(b).map(RangeFact::Constant).unwrap_or(RangeFact::Unknown),
                    _ => {
                        if (lf.is_positive() && rf.is_non_negative()) || (lf.is_non_negative() && rf.is_positive()) {
                            RangeFact::Positive
                        } else if lf.is_non_negative() && rf.is_non_negative() {
                            RangeFact::NonNegative
                        } else {
                            RangeFact::Unknown
                        }
                    }
                },
                BinOp::Sub => match (lf.as_constant(), rf.as_constant()) {
                    (Some(a), Some(b)) => a.checked_sub(b).map(RangeFact::Constant).unwrap_or(RangeFact::Unknown),
                    _ => RangeFact::Unknown,
                },
                BinOp::Mul => match (lf.as_constant(), rf.as_constant()) {
                    (Some(a), Some(b)) => a.checked_mul(b).map(RangeFact::Constant).unwrap_or(RangeFact::Unknown),
                    _ => {
                        if lf.is_positive() && rf.is_positive() {
                            RangeFact::Positive
                        } else if lf.is_non_negative() && rf.is_non_negative() {
                            RangeFact::NonNegative
                        } else {
                            RangeFact::Unknown
                        }
                    }
                },
                BinOp::Div | BinOp::DivU => match (lf.as_constant(), rf.as_constant()) {
                    (Some(a), Some(b)) if b != 0 && !(a == i64::MIN && b == -1) => {
                        RangeFact::Constant(a / b)
                    }
                    _ => {
                        if lf.is_non_negative() && rf.is_positive() {
                            RangeFact::NonNegative
                        } else {
                            RangeFact::Unknown
                        }
                    }
                },
                BinOp::Rem | BinOp::RemU => match (lf.as_constant(), rf.as_constant()) {
                    (Some(a), Some(b)) if b != 0 && !(a == i64::MIN && b == -1) => {
                        RangeFact::Constant(a % b)
                    }
                    _ => {
                        if lf.is_non_negative() {
                            if let Some(b) = rf.as_constant() {
                                if b > 0 {
                                    RangeFact::ClosedRange(0, b - 1)
                                } else {
                                    RangeFact::NonNegative
                                }
                            } else {
                                RangeFact::NonNegative
                            }
                        } else {
                            RangeFact::Unknown
                        }
                    }
                },
                BinOp::And => {
                    if let Some(k) = rf.as_constant() {
                        if k >= 0 {
                            return RangeFact::ClosedRange(0, k);
                        }
                    }
                    if let Some(k) = lf.as_constant() {
                        if k >= 0 {
                            return RangeFact::ClosedRange(0, k);
                        }
                    }
                    if lf.is_non_negative() || rf.is_non_negative() {
                        RangeFact::NonNegative
                    } else {
                        RangeFact::Unknown
                    }
                }
                BinOp::Shr | BinOp::ShrU => {
                    if lf.is_non_negative() {
                        RangeFact::NonNegative
                    } else {
                        RangeFact::Unknown
                    }
                }
                _ => RangeFact::Unknown,
            }
        }
        WirExpr::Unary { op, kind: Kind::I64, arg } => {
            let af = compute_fact(arg, env);
            match op {
                UnOp::Neg => match af.as_constant() {
                    Some(c) => c.checked_neg().map(RangeFact::Constant).unwrap_or(RangeFact::Unknown),
                    None => RangeFact::Unknown,
                },
                _ => RangeFact::Unknown,
            }
        }
        _ => RangeFact::Unknown,
    }
}

/// Extract assertions about variables from a branch condition.
fn extract_facts(
    cond: &WirExpr,
    is_true: bool,
    env: &HashMap<String, RangeFact>,
) -> Vec<(String, RangeFact)> {
    let mut facts = Vec::new();
    match cond {
        WirExpr::Unary { op: UnOp::Not, arg, .. } => {
            return extract_facts(arg, !is_true, env);
        }
        WirExpr::Binary { op, kind: Kind::I64, lhs, rhs } => {
            // Check GetLocal vs ConstI64
            if let (WirExpr::GetLocal(v), Some(k)) = (lhs.as_ref(), compute_fact(rhs, env).as_constant()) {
                if let Some(f) = fact_from_relop(*op, k, is_true) {
                    facts.push((v.clone(), f));
                }
            } else if let (Some(k), WirExpr::GetLocal(v)) = (compute_fact(lhs, env).as_constant(), rhs.as_ref()) {
                // k < v  <=>  v > k
                if let Some(mirrored_op) = mirror_relop(*op) {
                    if let Some(f) = fact_from_relop(mirrored_op, k, is_true) {
                        facts.push((v.clone(), f));
                    }
                }
            }
        }
        WirExpr::Binary { op: BinOp::Eq, kind: Kind::I32, lhs, rhs } => {
            if let (sub_cond, WirExpr::ConstI32(0)) = (lhs.as_ref(), rhs.as_ref()) {
                // cond == 0 is effectively !cond
                return extract_facts(sub_cond, !is_true, env);
            }
        }
        _ => {}
    }
    facts
}

fn mirror_relop(op: BinOp) -> Option<BinOp> {
    match op {
        BinOp::Lt => Some(BinOp::Gt),
        BinOp::Le => Some(BinOp::Ge),
        BinOp::Gt => Some(BinOp::Lt),
        BinOp::Ge => Some(BinOp::Le),
        BinOp::Eq => Some(BinOp::Eq),
        BinOp::Ne => Some(BinOp::Ne),
        _ => None,
    }
}

fn fact_from_relop(op: BinOp, k: i64, is_true: bool) -> Option<RangeFact> {
    let effective_op = if is_true { op } else { op.invert()? };
    match effective_op {
        BinOp::Gt | BinOp::GtU => {
            let lo = k.saturating_add(1);
            Some(RangeFact::from_bounds(lo, i64::MAX))
        }
        BinOp::Ge | BinOp::GeU => {
            Some(RangeFact::from_bounds(k, i64::MAX))
        }
        BinOp::Lt | BinOp::LtU => {
            let hi = k.saturating_sub(1);
            Some(RangeFact::from_bounds(i64::MIN, hi))
        }
        BinOp::Le | BinOp::LeU => {
            Some(RangeFact::from_bounds(i64::MIN, k))
        }
        BinOp::Eq => Some(RangeFact::Constant(k)),
        BinOp::Ne => None,
        _ => None,
    }
}

fn find_mutated_locals(seq: &[WirNode], out: &mut std::collections::HashSet<String>) {
    for node in seq {
        match node {
            WirNode::SetLocal { local, .. } => {
                out.insert(local.clone());
            }
            WirNode::Source { body, .. } | WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
                find_mutated_locals(body, out);
            }
            WirNode::If { then_, els, .. } => {
                find_mutated_locals(then_, out);
                find_mutated_locals(els, out);
            }
            WirNode::CallStoreMulti { dests, .. } | WirNode::CallIndirectStoreMulti { dests, .. } => {
                for d in dests {
                    out.insert(d.clone());
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wir::{WirFunc, WirLocal, WirTy};

    #[test]
    fn test_range_lattice_operations() {
        let u = RangeFact::Unknown;
        let pos = RangeFact::Positive;
        let non_neg = RangeFact::NonNegative;
        let c5 = RangeFact::Constant(5);
        let c_neg3 = RangeFact::Constant(-3);
        let r10_20 = RangeFact::ClosedRange(10, 20);

        assert!(pos.is_non_negative());
        assert!(pos.is_positive());
        assert!(non_neg.is_non_negative());
        assert!(!non_neg.is_positive());
        assert!(c5.is_non_negative());
        assert!(c5.is_positive());
        assert!(!c_neg3.is_non_negative());
        assert!(!c_neg3.is_positive());
        assert!(r10_20.is_non_negative());
        assert!(r10_20.is_positive());

        // Meet
        assert_eq!(u.meet(&pos), pos);
        assert_eq!(non_neg.meet(&pos), pos);
        assert_eq!(r10_20.meet(&c5), RangeFact::Unknown); // disjoint
        assert_eq!(r10_20.meet(&RangeFact::ClosedRange(15, 25)), RangeFact::ClosedRange(15, 20));

        // Join
        assert_eq!(u.join(&pos), u);
        assert_eq!(pos.join(&non_neg), non_neg);
        assert_eq!(RangeFact::ClosedRange(1, 5).join(&RangeFact::ClosedRange(3, 10)), RangeFact::ClosedRange(1, 10));
    }

    #[test]
    fn test_power_of_two_division_simplification() {
        let mut func = WirFunc {
            name: "test_div".into(),
            params: vec![WirLocal { name: "x".into(), ty: WirTy::Int }],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![
                // if x >= 0: return x / 8
                WirNode::If {
                    cond: WirExpr::Binary {
                        op: BinOp::Ge,
                        kind: Kind::I64,
                        lhs: Box::new(WirExpr::GetLocal("x".into())),
                        rhs: Box::new(WirExpr::ConstI64(0)),
                    },
                    then_: vec![
                        WirNode::Push(WirExpr::Binary {
                            op: BinOp::Div,
                            kind: Kind::I64,
                            lhs: Box::new(WirExpr::GetLocal("x".into())),
                            rhs: Box::new(WirExpr::ConstI64(8)),
                        }),
                    ],
                    els: vec![
                        WirNode::Push(WirExpr::ConstI64(0)),
                    ],
                    result: Some(WirTy::Int),
                },
            ],
            raw_body: None,
        };

        simplify_func(&mut func);

        if let WirNode::If { then_, .. } = &func.body[0] {
            if let WirNode::Push(WirExpr::Binary { op, rhs, .. }) = &then_[0] {
                assert_eq!(*op, BinOp::Shr, "Division by 8 on non-negative x should become shift right");
                assert!(matches!(**rhs, WirExpr::ConstI64(3)), "Shift amount should be 3");
            } else {
                panic!("Expected Push(Binary)");
            }
        } else {
            panic!("Expected If node");
        }
    }

    #[test]
    fn test_power_of_two_rem_simplification() {
        let mut func = WirFunc {
            name: "test_rem".into(),
            params: vec![WirLocal { name: "x".into(), ty: WirTy::Int }],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![
                // if x > 0: return x % 16
                WirNode::If {
                    cond: WirExpr::Binary {
                        op: BinOp::Gt,
                        kind: Kind::I64,
                        lhs: Box::new(WirExpr::GetLocal("x".into())),
                        rhs: Box::new(WirExpr::ConstI64(0)),
                    },
                    then_: vec![
                        WirNode::Push(WirExpr::Binary {
                            op: BinOp::Rem,
                            kind: Kind::I64,
                            lhs: Box::new(WirExpr::GetLocal("x".into())),
                            rhs: Box::new(WirExpr::ConstI64(16)),
                        }),
                    ],
                    els: vec![
                        WirNode::Push(WirExpr::ConstI64(0)),
                    ],
                    result: Some(WirTy::Int),
                },
            ],
            raw_body: None,
        };

        simplify_func(&mut func);

        if let WirNode::If { then_, .. } = &func.body[0] {
            if let WirNode::Push(WirExpr::Binary { op, rhs, .. }) = &then_[0] {
                assert_eq!(*op, BinOp::And, "Remainder by 16 on positive x should become bitwise AND");
                assert!(matches!(**rhs, WirExpr::ConstI64(15)), "Mask should be 15");
            } else {
                panic!("Expected Push(Binary)");
            }
        } else {
            panic!("Expected If node");
        }
    }

    #[test]
    fn test_rem_zero_equality_fusion() {
        let mut func = WirFunc {
            name: "test_fuse".into(),
            params: vec![WirLocal { name: "x".into(), ty: WirTy::Int }],
            ret: vec![WirTy::Bool],
            locals: vec![],
            body: vec![
                WirNode::Push(WirExpr::Binary {
                    op: BinOp::Eq,
                    kind: Kind::I32,
                    lhs: Box::new(WirExpr::Binary {
                        op: BinOp::Rem,
                        kind: Kind::I64,
                        lhs: Box::new(WirExpr::GetLocal("x".into())),
                        rhs: Box::new(WirExpr::ConstI64(2)),
                    }),
                    rhs: Box::new(WirExpr::ConstI64(0)),
                }),
            ],
            raw_body: None,
        };

        simplify_func(&mut func);

        if let WirNode::Push(WirExpr::Binary { op, lhs, .. }) = &func.body[0] {
            assert_eq!(*op, BinOp::Eq);
            if let WirExpr::Binary { op: inner_op, rhs: mask_rhs, .. } = lhs.as_ref() {
                assert_eq!(*inner_op, BinOp::And);
                assert!(matches!(**mask_rhs, WirExpr::ConstI64(1)));
            } else {
                panic!("Expected inner Binary(And)");
            }
        } else {
            panic!("Expected Push(Binary)");
        }
    }

    #[test]
    fn test_negative_rem_is_not_simplified_to_and() {
        // Without a non-negative proof, signed remainder by power of 2 cannot simply be replaced by AND
        let mut func = WirFunc {
            name: "test_signed_rem".into(),
            params: vec![WirLocal { name: "x".into(), ty: WirTy::Int }],
            ret: vec![WirTy::Int],
            locals: vec![],
            body: vec![
                WirNode::Push(WirExpr::Binary {
                    op: BinOp::Rem,
                    kind: Kind::I64,
                    lhs: Box::new(WirExpr::GetLocal("x".into())),
                    rhs: Box::new(WirExpr::ConstI64(8)),
                }),
            ],
            raw_body: None,
        };

        simplify_func(&mut func);

        if let WirNode::Push(WirExpr::Binary { op, .. }) = &func.body[0] {
            assert_eq!(*op, BinOp::Rem, "Signed remainder on unknown sign must remain Rem");
        } else {
            panic!("Expected Push(Binary)");
        }
    }

    #[test]
    fn test_zero_divisor_and_overflow_traps_are_preserved() {
        // Division or remainder by zero must not panic in compiler and must not be eliminated
        let mut func = WirFunc {
            name: "test_traps".into(),
            params: vec![],
            ret: vec![WirTy::Int, WirTy::Int, WirTy::Int],
            locals: vec![],
            body: vec![
                WirNode::Push(WirExpr::Binary {
                    op: BinOp::Div,
                    kind: Kind::I64,
                    lhs: Box::new(WirExpr::ConstI64(42)),
                    rhs: Box::new(WirExpr::ConstI64(0)),
                }),
                WirNode::Push(WirExpr::Binary {
                    op: BinOp::Rem,
                    kind: Kind::I64,
                    lhs: Box::new(WirExpr::ConstI64(42)),
                    rhs: Box::new(WirExpr::ConstI64(0)),
                }),
                WirNode::Push(WirExpr::Binary {
                    op: BinOp::Div,
                    kind: Kind::I64,
                    lhs: Box::new(WirExpr::ConstI64(i64::MIN)),
                    rhs: Box::new(WirExpr::ConstI64(-1)),
                }),
            ],
            raw_body: None,
        };

        simplify_func(&mut func);

        assert_eq!(func.body.len(), 3);
        // All trap operations must remain intact
        if let WirNode::Push(WirExpr::Binary { op, rhs, .. }) = &func.body[0] {
            assert_eq!(*op, BinOp::Div);
            assert!(matches!(**rhs, WirExpr::ConstI64(0)));
        } else {
            panic!("Expected Push(Div)");
        }
        if let WirNode::Push(WirExpr::Binary { op, rhs, .. }) = &func.body[1] {
            assert_eq!(*op, BinOp::Rem);
            assert!(matches!(**rhs, WirExpr::ConstI64(0)));
        } else {
            panic!("Expected Push(Rem)");
        }
        if let WirNode::Push(WirExpr::Binary { op, lhs, rhs, .. }) = &func.body[2] {
            assert_eq!(*op, BinOp::Div);
            assert!(matches!(**lhs, WirExpr::ConstI64(i64::MIN)));
            assert!(matches!(**rhs, WirExpr::ConstI64(-1)));
        } else {
            panic!("Expected Push(Div)");
        }
    }

    #[test]
    fn test_exhaustive_mathematical_equivalence_vectors() {
        // Test mathematical contracts across a large vector of representative signed 64-bit values
        let test_values: &[i64] = &[
            0, 1, 2, 3, 4, 7, 8, 15, 16, 31, 32, 63, 64, 127, 128, 255, 256,
            1023, 1024, 65535, 65536,
            i64::MAX - 1, i64::MAX,
            -1, -2, -3, -4, -7, -8, -15, -16, -31, -32, -63, -64,
            -127, -128, -255, -256, -1023, -1024,
            i64::MIN + 1, i64::MIN,
        ];

        let powers_of_two: &[i64] = &[1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 1 << 30, 1 << 62];

        for &val in test_values {
            for &p in powers_of_two {
                let mask = p - 1;
                let shift = p.trailing_zeros();

                // Contract 1: (x % 2^k == 0) <=> (x & (2^k - 1) == 0) for ALL x in i64
                let mod_is_zero = (val % p) == 0;
                let and_is_zero = (val & mask) == 0;
                assert_eq!(mod_is_zero, and_is_zero, "Modulo 0-test mismatch for val={val}, p={p}");

                // Contract 2: When x >= 0: x % 2^k == x & (2^k - 1)
                if val >= 0 {
                    assert_eq!(val % p, val & mask, "Non-negative remainder mismatch for val={val}, p={p}");
                    assert_eq!(val / p, val >> shift, "Non-negative division mismatch for val={val}, p={p}");
                }
            }
        }
    }
}
