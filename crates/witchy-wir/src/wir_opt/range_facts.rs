//! Abstract Range Facts Lattice and Branch Refinement (RFC-0153 Track 2).
//!
//! Provides abstract fact inference for typed WIR locals:
//! - Lattice: Unknown, Constant, Interval, NonNegative, Positive, Congruence.
//! - Wrap-aware transfer functions for integer arithmetic, bitwise operations, and copies.
//! - Dominated branch condition refinement.
//! - Bounded fixed-point loop widening operator.
//! - Deterministic diagnostic statistics tracking.

use std::collections::HashMap;
use crate::wir::{BinOp, UnOp, WirExpr};

/// Deterministic counters for range fact lifecycle events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RangeFactStats {
    pub facts_created: usize,
    pub facts_joined: usize,
    pub facts_widened: usize,
    pub facts_dropped: usize,
    pub facts_refined: usize,
}

/// Abstract range and numerical property lattice for 64-bit integer values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeFact {
    /// No range information or unconstrained value.
    Unknown,
    /// Known exact constant value.
    Constant(i64),
    /// Known closed interval `[lo, hi]` where `lo <= hi`.
    /// `no_wrap` indicates whether arithmetic producing this bound was proved overflow-free.
    Interval {
        lo: i64,
        hi: i64,
        no_wrap: bool,
    },
    /// Known non-negative value (`x >= 0`), with optional upper bound.
    NonNegative {
        upper: Option<i64>,
        no_wrap: bool,
    },
    /// Known strictly positive value (`x >= 1`), with optional upper bound.
    Positive {
        upper: Option<i64>,
        no_wrap: bool,
    },
    /// Known modular congruence `x = residue (mod modulus)` with optional sub-interval.
    Congruence {
        modulus: u64,
        residue: u64,
        interval: Option<Box<RangeFact>>,
    },
}

impl RangeFact {
    /// Constructs a constant fact.
    pub fn from_constant(c: i64) -> Self {
        RangeFact::Constant(c)
    }

    /// Constructs an interval or specialized fact from lower and upper bounds.
    pub fn from_bounds(lo: i64, hi: i64, no_wrap: bool) -> Self {
        if lo > hi {
            RangeFact::Unknown
        } else if lo == hi {
            RangeFact::Constant(lo)
        } else if lo == 0 && hi == i64::MAX {
            RangeFact::NonNegative { upper: None, no_wrap }
        } else if lo == 1 && hi == i64::MAX {
            RangeFact::Positive { upper: None, no_wrap }
        } else if lo == 0 {
            RangeFact::NonNegative { upper: Some(hi), no_wrap }
        } else if lo >= 1 {
            RangeFact::Positive { upper: Some(hi), no_wrap }
        } else {
            RangeFact::Interval { lo, hi, no_wrap }
        }
    }

    /// Returns true if this value is provably non-negative (`x >= 0`).
    pub fn is_non_negative(&self) -> bool {
        match self {
            RangeFact::Constant(c) => *c >= 0,
            RangeFact::Positive { .. } | RangeFact::NonNegative { .. } => true,
            RangeFact::Interval { lo, .. } => *lo >= 0,
            RangeFact::Congruence { interval: Some(inner), .. } => inner.is_non_negative(),
            RangeFact::Congruence { .. } | RangeFact::Unknown => false,
        }
    }

    /// Returns true if this value is provably strictly positive (`x >= 1`).
    pub fn is_positive(&self) -> bool {
        match self {
            RangeFact::Constant(c) => *c > 0,
            RangeFact::Positive { .. } => true,
            RangeFact::NonNegative { .. } => false,
            RangeFact::Interval { lo, .. } => *lo >= 1,
            RangeFact::Congruence { interval: Some(inner), .. } => inner.is_positive(),
            RangeFact::Congruence { .. } | RangeFact::Unknown => false,
        }
    }

    /// Returns true if this value is provably strictly negative (`x < 0`).
    pub fn is_strictly_negative(&self) -> bool {
        match self {
            RangeFact::Constant(c) => *c < 0,
            RangeFact::Interval { hi, .. } => *hi < 0,
            RangeFact::Congruence { interval: Some(inner), .. } => inner.is_strictly_negative(),
            RangeFact::Positive { .. } | RangeFact::NonNegative { .. } | RangeFact::Congruence { .. } | RangeFact::Unknown => false,
        }
    }

    /// Returns the exact constant value if known.
    pub fn as_constant(&self) -> Option<i64> {
        match self {
            RangeFact::Constant(c) => Some(*c),
            RangeFact::Interval { lo, hi, .. } if lo == hi => Some(*lo),
            RangeFact::Congruence { interval: Some(inner), .. } => inner.as_constant(),
            _ => None,
        }
    }

    /// Returns the lower bound if known.
    pub fn lo_bound(&self) -> Option<i64> {
        match self {
            RangeFact::Constant(c) => Some(*c),
            RangeFact::Positive { .. } => Some(1),
            RangeFact::NonNegative { .. } => Some(0),
            RangeFact::Interval { lo, .. } => Some(*lo),
            RangeFact::Congruence { interval: Some(inner), .. } => inner.lo_bound(),
            RangeFact::Congruence { .. } | RangeFact::Unknown => None,
        }
    }

    /// Returns the upper bound if known.
    pub fn hi_bound(&self) -> Option<i64> {
        match self {
            RangeFact::Constant(c) => Some(*c),
            RangeFact::Positive { upper, .. } | RangeFact::NonNegative { upper, .. } => {
                upper.or(Some(i64::MAX))
            }
            RangeFact::Interval { hi, .. } => Some(*hi),
            RangeFact::Congruence { interval: Some(inner), .. } => inner.hi_bound(),
            RangeFact::Congruence { .. } | RangeFact::Unknown => None,
        }
    }

    /// Returns whether the fact has proof of wrap freedom.
    pub fn is_no_wrap(&self) -> bool {
        match self {
            RangeFact::Constant(_) => true,
            RangeFact::Interval { no_wrap, .. }
            | RangeFact::NonNegative { no_wrap, .. }
            | RangeFact::Positive { no_wrap, .. } => *no_wrap,
            RangeFact::Congruence { interval: Some(inner), .. } => inner.is_no_wrap(),
            RangeFact::Congruence { .. } | RangeFact::Unknown => false,
        }
    }

    /// Greatest lower bound (intersection / meet) along the same control flow path.
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
        let no_wrap = self.is_no_wrap() || other.is_no_wrap();

        match (lo, hi) {
            (Some(l), Some(h)) => {
                if l > h {
                    RangeFact::Unknown
                } else {
                    RangeFact::from_bounds(l, h, no_wrap)
                }
            }
            (Some(l), None) => {
                if l >= 1 {
                    RangeFact::Positive { upper: None, no_wrap }
                } else if l >= 0 {
                    RangeFact::NonNegative { upper: None, no_wrap }
                } else {
                    RangeFact::Interval { lo: l, hi: i64::MAX, no_wrap }
                }
            }
            (None, Some(h)) => RangeFact::Interval { lo: i64::MIN, hi: h, no_wrap },
            (None, None) => RangeFact::Unknown,
        }
    }

    /// Least upper bound (union / join) across control flow join points.
    pub fn join(&self, other: &RangeFact) -> Self {
        if *self == RangeFact::Unknown || *other == RangeFact::Unknown {
            return RangeFact::Unknown;
        }
        if self == other {
            return self.clone();
        }

        let lo = match (self.lo_bound(), other.lo_bound()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            _ => None,
        };
        let hi = match (self.hi_bound(), other.hi_bound()) {
            (Some(a), Some(b)) => Some(a.max(b)),
            _ => None,
        };
        let no_wrap = self.is_no_wrap() && other.is_no_wrap();

        match (lo, hi) {
            (Some(l), Some(h)) => RangeFact::from_bounds(l, h, no_wrap),
            (Some(l), None) => {
                if l >= 1 {
                    RangeFact::Positive { upper: None, no_wrap }
                } else if l >= 0 {
                    RangeFact::NonNegative { upper: None, no_wrap }
                } else {
                    RangeFact::Unknown
                }
            }
            _ => RangeFact::Unknown,
        }
    }

    /// Bounded fixed-point loop widening operator.
    /// Widens growing bounds to infinity to guarantee convergence.
    pub fn widen(&self, next: &RangeFact) -> Self {
        if *self == RangeFact::Unknown || *next == RangeFact::Unknown {
            return RangeFact::Unknown;
        }
        if self == next {
            return self.clone();
        }

        let lo = match (self.lo_bound(), next.lo_bound()) {
            (Some(a), Some(b)) => {
                if b < a {
                    None // Widen downwards
                } else {
                    Some(a) // Stable lower bound
                }
            }
            _ => None,
        };

        let hi = match (self.hi_bound(), next.hi_bound()) {
            (Some(a), Some(b)) => {
                if b > a {
                    None // Widen upwards
                } else {
                    Some(a) // Stable upper bound
                }
            }
            _ => None,
        };

        let no_wrap = self.is_no_wrap() && next.is_no_wrap();

        match (lo, hi) {
            (Some(l), Some(h)) => RangeFact::from_bounds(l, h, no_wrap),
            (Some(l), None) => {
                if l >= 1 {
                    RangeFact::Positive { upper: None, no_wrap }
                } else if l >= 0 {
                    RangeFact::NonNegative { upper: None, no_wrap }
                } else {
                    RangeFact::Unknown
                }
            }
            (None, Some(h)) => {
                if h < 0 {
                    RangeFact::Interval { lo: i64::MIN, hi: h, no_wrap }
                } else {
                    RangeFact::Unknown
                }
            }
            (None, None) => RangeFact::Unknown,
        }
    }
}

/// Environment mapping local variable names to known RangeFacts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RangeEnv {
    pub facts: HashMap<String, RangeFact>,
}

impl RangeEnv {
    pub fn new() -> Self {
        Self { facts: HashMap::new() }
    }

    pub fn get(&self, name: &str) -> RangeFact {
        self.facts.get(name).cloned().unwrap_or(RangeFact::Unknown)
    }

    pub fn set(&mut self, name: String, fact: RangeFact) {
        if fact == RangeFact::Unknown {
            self.facts.remove(&name);
        } else {
            self.facts.insert(name, fact);
        }
    }

    pub fn meet_fact(&mut self, name: &str, fact: &RangeFact) {
        let current = self.get(name);
        let refined = current.meet(fact);
        self.set(name.to_string(), refined);
    }

    pub fn join(&self, other: &RangeEnv) -> RangeEnv {
        let mut result = RangeEnv::new();
        for (k, v1) in &self.facts {
            if let Some(v2) = other.facts.get(k) {
                let joined = v1.join(v2);
                if joined != RangeFact::Unknown {
                    result.set(k.clone(), joined);
                }
            }
        }
        result
    }

    pub fn widen(&self, next: &RangeEnv) -> RangeEnv {
        let mut result = RangeEnv::new();
        for (k, v1) in &self.facts {
            if let Some(v2) = next.facts.get(k) {
                let widened = v1.widen(v2);
                if widened != RangeFact::Unknown {
                    result.set(k.clone(), widened);
                }
            }
        }
        result
    }
}

/// Computes the inferred RangeFact for an expression given an environment.
pub fn compute_fact(expr: &WirExpr, env: &RangeEnv) -> RangeFact {
    match expr {
        WirExpr::ConstI64(c) => RangeFact::Constant(*c),
        WirExpr::ConstI32(c) => RangeFact::Constant(*c as i64),
        WirExpr::GetLocal(name) => env.get(name),
        WirExpr::ToSlot(inner, _) | WirExpr::FromSlot(inner, _) | WirExpr::Convert { arg: inner, .. } => {
            compute_fact(inner, env)
        }
        WirExpr::Binary { op, lhs, rhs, .. } => {
            let left = compute_fact(lhs, env);
            let right = compute_fact(rhs, env);
            transfer_binop(*op, &left, &right)
        }
        WirExpr::Unary { op, arg, .. } => {
            let inner_fact = compute_fact(arg, env);
            transfer_unop(*op, &inner_fact)
        }
        _ => RangeFact::Unknown,
    }
}

fn transfer_binop(op: BinOp, lhs: &RangeFact, rhs: &RangeFact) -> RangeFact {
    // If both are exact constants:
    if let (Some(a), Some(b)) = (lhs.as_constant(), rhs.as_constant()) {
        match op {
            BinOp::Add => {
                if let Some(res) = a.checked_add(b) {
                    return RangeFact::Constant(res);
                }
            }
            BinOp::Sub => {
                if let Some(res) = a.checked_sub(b) {
                    return RangeFact::Constant(res);
                }
            }
            BinOp::Mul => {
                if let Some(res) = a.checked_mul(b) {
                    return RangeFact::Constant(res);
                }
            }
            BinOp::Div if b != 0 => {
                if let Some(res) = a.checked_div(b) {
                    return RangeFact::Constant(res);
                }
            }
            BinOp::Rem if b != 0 => {
                if let Some(res) = a.checked_rem(b) {
                    return RangeFact::Constant(res);
                }
            }
            BinOp::And => return RangeFact::Constant(a & b),
            BinOp::Or => return RangeFact::Constant(a | b),
            BinOp::Xor => return RangeFact::Constant(a ^ b),
            BinOp::Shl if (0..64).contains(&b) => {
                if let Some(res) = a.checked_shl(b as u32) {
                    return RangeFact::Constant(res);
                }
            }
            BinOp::Shr if (0..64).contains(&b) => {
                return RangeFact::Constant(a >> (b as u32));
            }
            BinOp::ShrU if (0..64).contains(&b) => {
                return RangeFact::Constant(((a as u64) >> (b as u32)) as i64);
            }
            BinOp::Eq => return RangeFact::Constant(if a == b { 1 } else { 0 }),
            BinOp::Ne => return RangeFact::Constant(if a != b { 1 } else { 0 }),
            BinOp::Lt => return RangeFact::Constant(if a < b { 1 } else { 0 }),
            BinOp::Le => return RangeFact::Constant(if a <= b { 1 } else { 0 }),
            BinOp::Gt => return RangeFact::Constant(if a > b { 1 } else { 0 }),
            BinOp::Ge => return RangeFact::Constant(if a >= b { 1 } else { 0 }),
            _ => {}
        }
    }

    match op {
        BinOp::Add => {
            if let (Some(l_lo), Some(l_hi), Some(r_lo), Some(r_hi)) =
                (lhs.lo_bound(), lhs.hi_bound(), rhs.lo_bound(), rhs.hi_bound())
            {
                if let (Some(new_lo), Some(new_hi)) = (l_lo.checked_add(r_lo), l_hi.checked_add(r_hi)) {
                    let no_wrap = lhs.is_no_wrap() && rhs.is_no_wrap();
                    return RangeFact::from_bounds(new_lo, new_hi, no_wrap);
                }
            }
            if lhs.is_non_negative() && rhs.is_non_negative() {
                return RangeFact::NonNegative {
                    upper: None,
                    no_wrap: lhs.is_no_wrap() && rhs.is_no_wrap(),
                };
            }
            RangeFact::Unknown
        }
        BinOp::Sub => {
            if let (Some(l_lo), Some(l_hi), Some(r_lo), Some(r_hi)) =
                (lhs.lo_bound(), lhs.hi_bound(), rhs.lo_bound(), rhs.hi_bound())
            {
                if let (Some(new_lo), Some(new_hi)) = (l_lo.checked_sub(r_hi), l_hi.checked_sub(r_lo)) {
                    let no_wrap = lhs.is_no_wrap() && rhs.is_no_wrap();
                    return RangeFact::from_bounds(new_lo, new_hi, no_wrap);
                }
            }
            RangeFact::Unknown
        }
        BinOp::Mul => {
            // Check multiplication by positive constant
            if let Some(c) = rhs.as_constant() {
                if c > 0 {
                    if let (Some(l_lo), Some(l_hi)) = (lhs.lo_bound(), lhs.hi_bound()) {
                        if let (Some(new_lo), Some(new_hi)) = (l_lo.checked_mul(c), l_hi.checked_mul(c)) {
                            return RangeFact::from_bounds(new_lo, new_hi, lhs.is_no_wrap());
                        }
                    }
                    if lhs.is_non_negative() {
                        return RangeFact::NonNegative { upper: None, no_wrap: lhs.is_no_wrap() };
                    }
                }
            }
            if let Some(c) = lhs.as_constant() {
                if c > 0 {
                    if let (Some(r_lo), Some(r_hi)) = (rhs.lo_bound(), rhs.hi_bound()) {
                        if let (Some(new_lo), Some(new_hi)) = (r_lo.checked_mul(c), r_hi.checked_mul(c)) {
                            return RangeFact::from_bounds(new_lo, new_hi, rhs.is_no_wrap());
                        }
                    }
                    if rhs.is_non_negative() {
                        return RangeFact::NonNegative { upper: None, no_wrap: rhs.is_no_wrap() };
                    }
                }
            }
            RangeFact::Unknown
        }
        BinOp::Div => {
            if let Some(d) = rhs.as_constant() {
                if d > 0 {
                    if let (Some(l_lo), Some(l_hi)) = (lhs.lo_bound(), lhs.hi_bound()) {
                        let new_lo = l_lo / d;
                        let new_hi = l_hi / d;
                        return RangeFact::from_bounds(new_lo, new_hi, lhs.is_no_wrap());
                    }
                    if lhs.is_non_negative() {
                        return RangeFact::NonNegative { upper: None, no_wrap: lhs.is_no_wrap() };
                    }
                }
            }
            RangeFact::Unknown
        }
        BinOp::Rem => {
            if let Some(m) = rhs.as_constant() {
                if m > 0 {
                    if lhs.is_non_negative() {
                        let max_hi = m - 1;
                        let hi = match lhs.hi_bound() {
                            Some(h) => h.min(max_hi),
                            None => max_hi,
                        };
                        return RangeFact::Congruence {
                            modulus: m as u64,
                            residue: 0, // General mod m
                            interval: Some(Box::new(RangeFact::from_bounds(0, hi, true))),
                        };
                    }
                }
            }
            RangeFact::Unknown
        }
        BinOp::And => {
            // Masking with non-negative constant
            if let Some(mask) = rhs.as_constant() {
                if mask >= 0 {
                    let hi = match lhs.hi_bound() {
                        Some(h) if lhs.is_non_negative() => h.min(mask),
                        _ => mask,
                    };
                    return RangeFact::from_bounds(0, hi, true);
                }
            }
            if let Some(mask) = lhs.as_constant() {
                if mask >= 0 {
                    let hi = match rhs.hi_bound() {
                        Some(h) if rhs.is_non_negative() => h.min(mask),
                        _ => mask,
                    };
                    return RangeFact::from_bounds(0, hi, true);
                }
            }
            if lhs.is_non_negative() || rhs.is_non_negative() {
                return RangeFact::NonNegative { upper: None, no_wrap: true };
            }
            RangeFact::Unknown
        }
        BinOp::Shr => {
            if let Some(shift) = rhs.as_constant() {
                if (0..64).contains(&shift) {
                    if let (Some(l_lo), Some(l_hi)) = (lhs.lo_bound(), lhs.hi_bound()) {
                        return RangeFact::from_bounds(l_lo >> shift, l_hi >> shift, lhs.is_no_wrap());
                    }
                    if lhs.is_non_negative() {
                        return RangeFact::NonNegative { upper: None, no_wrap: lhs.is_no_wrap() };
                    }
                }
            }
            RangeFact::Unknown
        }
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            RangeFact::from_bounds(0, 1, true)
        }
        _ => RangeFact::Unknown,
    }
}

fn transfer_unop(op: UnOp, inner: &RangeFact) -> RangeFact {
    match op {
        UnOp::Neg => {
            if let (Some(lo), Some(hi)) = (inner.lo_bound(), inner.hi_bound()) {
                if let (Some(new_lo), Some(new_hi)) = (hi.checked_neg(), lo.checked_neg()) {
                    return RangeFact::from_bounds(new_lo, new_hi, inner.is_no_wrap());
                }
            }
            RangeFact::Unknown
        }
        UnOp::Clz | UnOp::Ctz | UnOp::Popcnt => RangeFact::from_bounds(0, 64, true),
        _ => RangeFact::Unknown,
    }
}

/// Refines local variable facts from a dominating condition expression.
/// Returns a list of variable names to refined facts when `is_true` branch is taken.
pub fn refine_condition(cond: &WirExpr, is_true: bool, env: &RangeEnv) -> Vec<(String, RangeFact)> {
    let mut refined = Vec::new();
    match cond {
        WirExpr::Binary { op, lhs, rhs, .. } => {
            let left_fact = compute_fact(lhs, env);
            let right_fact = compute_fact(rhs, env);

            match (lhs.as_ref(), rhs.as_ref()) {
                (WirExpr::GetLocal(var), _) => {
                    refine_var_op_fact(var, *op, &right_fact, is_true, &mut refined);
                }
                (_, WirExpr::GetLocal(var)) => {
                    // Invert op for symmetric comparison
                    if let Some(inv_op) = invert_comparison_direction(*op) {
                        refine_var_op_fact(var, inv_op, &left_fact, is_true, &mut refined);
                    }
                }
                _ => {}
            }

            // If both sides are variables and equal:
            if let (WirExpr::GetLocal(v1), WirExpr::GetLocal(v2)) = (lhs.as_ref(), rhs.as_ref()) {
                if (*op == BinOp::Eq && is_true) || (*op == BinOp::Ne && !is_true) {
                    let common = left_fact.meet(&right_fact);
                    refined.push((v1.clone(), common.clone()));
                    refined.push((v2.clone(), common));
                }
            }
        }
        WirExpr::GetLocal(var) => {
            // Truthy check: if `var` is tested directly as bool
            if is_true {
                refined.push((var.clone(), RangeFact::from_bounds(1, i64::MAX, true)));
            } else {
                refined.push((var.clone(), RangeFact::Constant(0)));
            }
        }
        _ => {}
    }
    refined
}

fn invert_comparison_direction(op: BinOp) -> Option<BinOp> {
    match op {
        BinOp::Eq => Some(BinOp::Eq),
        BinOp::Ne => Some(BinOp::Ne),
        BinOp::Lt => Some(BinOp::Gt),
        BinOp::Le => Some(BinOp::Ge),
        BinOp::Gt => Some(BinOp::Lt),
        BinOp::Ge => Some(BinOp::Le),
        _ => None,
    }
}

fn refine_var_op_fact(
    var: &str,
    op: BinOp,
    rhs_fact: &RangeFact,
    is_true: bool,
    out: &mut Vec<(String, RangeFact)>,
) {
    let effective_op = if is_true { op } else {
        match op {
            BinOp::Eq => BinOp::Ne,
            BinOp::Ne => BinOp::Eq,
            BinOp::Lt => BinOp::Ge,
            BinOp::Le => BinOp::Gt,
            BinOp::Gt => BinOp::Le,
            BinOp::Ge => BinOp::Lt,
            _ => return,
        }
    };

    match effective_op {
        BinOp::Eq => {
            out.push((var.to_string(), rhs_fact.clone()));
        }
        BinOp::Lt => {
            if let Some(hi) = rhs_fact.hi_bound() {
                if let Some(bound) = hi.checked_sub(1) {
                    out.push((var.to_string(), RangeFact::Interval { lo: i64::MIN, hi: bound, no_wrap: true }));
                }
            }
        }
        BinOp::Le => {
            if let Some(hi) = rhs_fact.hi_bound() {
                out.push((var.to_string(), RangeFact::Interval { lo: i64::MIN, hi, no_wrap: true }));
            }
        }
        BinOp::Gt => {
            if let Some(lo) = rhs_fact.lo_bound() {
                if let Some(bound) = lo.checked_add(1) {
                    out.push((var.to_string(), RangeFact::Interval { lo: bound, hi: i64::MAX, no_wrap: true }));
                }
            }
        }
        BinOp::Ge => {
            if let Some(lo) = rhs_fact.lo_bound() {
                out.push((var.to_string(), RangeFact::Interval { lo, hi: i64::MAX, no_wrap: true }));
            }
        }
        _ => {}
    }
}
