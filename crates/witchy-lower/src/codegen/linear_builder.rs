//! RFC-0151 Linear build-plan infrastructure.
//!
//! A `LinearBuildPlan` is derived from typed ownership, layout, range, and
//! escape facts. It applies across eligible contiguous buffers (`Bytes`, `String`,
//! and packed sequences) whose intrinsic metadata declares the same operation
//! class (`ContiguousRead`, `ContiguousWrite`, `AppendScalar`, `AppendSlice`,
//! `Reserve`, `Finish`).
//!
//! No benchmark name, source filename, biological alphabet, output record, or
//! specific standard-library method is an optimization input.

use super::*;
use witchy_syntax::intrinsics::{ContiguousOpClass, ScalarValidation};
use witchy_wir::layout::LayoutId;

/// The representation class of the contiguous buffer under construction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinearValueKind {
    String,
    Bytes,
    PackedSequence(LayoutId),
}

/// A proven integer value range bound for a named variable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RangeFact {
    pub var_name: String,
    pub min: i64,
    pub max: i64,
}

/// (RFC-0151) Typed linear build plan for contiguous owned buffers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinearBuildPlan {
    /// The unique root variable / place owning the buffer.
    pub root_place: String,
    /// The representation kind of the buffer payload.
    pub value_kind: LinearValueKind,
    /// Byte width of a single element (e.g. 1 for Bytes/String).
    pub payload_width: u32,
    /// Optional cached length register / local name.
    pub length_local: Option<String>,
    /// Optional cached capacity register / local name.
    pub capacity_local: Option<String>,
    /// Optional cached write cursor register / local name.
    pub cursor_local: Option<String>,
    /// The shadow ownership token (`__cap`) variable for in-place mutation.
    pub ownership_token: Option<String>,
    /// Proven integer range facts available within the builder's scope.
    pub validated_range_facts: Vec<RangeFact>,
    /// Monotonic epoch counter at plan creation, used for conservative escape invalidation.
    pub escape_epoch: u32,
}

impl LinearBuildPlan {
    /// Create a new linear build plan for a root place.
    pub fn new(
        root_place: String,
        value_kind: LinearValueKind,
        payload_width: u32,
        escape_epoch: u32,
    ) -> Self {
        Self {
            root_place,
            value_kind,
            payload_width,
            length_local: None,
            capacity_local: None,
            cursor_local: None,
            ownership_token: None,
            validated_range_facts: Vec::new(),
            escape_epoch,
        }
    }

    /// Attach register/local cache variable names.
    pub fn with_locals(
        mut self,
        length: Option<String>,
        capacity: Option<String>,
        cursor: Option<String>,
        ownership_token: Option<String>,
    ) -> Self {
        self.length_local = length;
        self.capacity_local = capacity;
        self.cursor_local = cursor;
        self.ownership_token = ownership_token;
        self
    }

    /// Check if this plan is still valid at the given escape epoch.
    pub fn is_valid_at_epoch(&self, current_epoch: u32) -> bool {
        self.escape_epoch == current_epoch
    }

    /// Record a proven range fact for a variable.
    pub fn record_range_fact(&mut self, var_name: String, min: i64, max: i64) {
        if let Some(existing) = self.validated_range_facts.iter_mut().find(|f| f.var_name == var_name) {
            existing.min = existing.min.max(min);
            existing.max = existing.max.min(max);
        } else {
            self.validated_range_facts.push(RangeFact { var_name, min, max });
        }
    }

    /// Check if a variable is proven to be within valid byte range `0..=255`.
    pub fn range_is_byte_proven(&self, var_name: &str) -> bool {
        self.validated_range_facts
            .iter()
            .any(|f| f.var_name == var_name && f.min >= 0 && f.max <= 255)
    }

    /// Whether this plan describes a direct byte buffer (`Bytes` with 1-byte elements).
    pub fn is_direct_byte_eligible(&self) -> bool {
        matches!(self.value_kind, LinearValueKind::Bytes) && self.payload_width == 1
    }

    /// Whether this plan describes a string builder (`String` with 1-byte UTF-8 elements).
    pub fn is_string_builder_eligible(&self) -> bool {
        matches!(self.value_kind, LinearValueKind::String) && self.payload_width == 1
    }

    /// Check if a contiguous operation class is applicable to this plan.
    pub fn contiguous_op_applicable(&self, op: ContiguousOpClass) -> bool {
        match op {
            ContiguousOpClass::ContiguousRead { width } | ContiguousOpClass::ContiguousWrite { width } => {
                self.payload_width == width
            }
            ContiguousOpClass::AppendScalar { width, validation } => {
                if self.payload_width != width {
                    return false;
                }
                match validation {
                    ScalarValidation::None => true,
                    ScalarValidation::ByteRange => matches!(self.value_kind, LinearValueKind::Bytes),
                    ScalarValidation::Utf8Scalar => matches!(self.value_kind, LinearValueKind::String),
                }
            }
            ContiguousOpClass::AppendSlice { width } => self.payload_width == width,
            ContiguousOpClass::Reserve | ContiguousOpClass::Finish => true,
        }
    }
}

#[allow(dead_code)]
impl<'types> Codegen<'types> {
    /// Note an active linear build plan for a root variable.
    pub(crate) fn note_linear_builder(&mut self, plan: LinearBuildPlan) {
        self.linear_build_plans.insert(plan.root_place.clone(), plan);
    }

    /// Invalidate all active linear build plans (advances the escape epoch).
    pub(crate) fn invalidate_linear_builders(&mut self) {
        self.linear_build_epoch = self.linear_build_epoch.wrapping_add(1);
        self.linear_build_plans.clear();
    }

    /// Query the active linear build plan for a root place, if valid at the current epoch.
    pub(crate) fn active_linear_build_plan(&self, root: &str) -> Option<&LinearBuildPlan> {
        self.linear_build_plans.get(root).filter(|p| p.is_valid_at_epoch(self.linear_build_epoch))
    }

    /// Mutably access the active linear build plan for a root place, if valid at the current epoch.
    pub(crate) fn active_linear_build_plan_mut(&mut self, root: &str) -> Option<&mut LinearBuildPlan> {
        let epoch = self.linear_build_epoch;
        self.linear_build_plans.get_mut(root).filter(|p| p.is_valid_at_epoch(epoch))
    }

    /// (RFC-0151) Direct byte read with bounds check and `i32.load8_u`.
    pub(crate) fn lower_bytes_at(
        &mut self,
        bytes_expr: &Expr,
        idx_expr: &Expr,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use super::Kind;
        use witchy_wir::wir::{BinOp, Kind as WirKind, WirExpr as W, WirNode as N};
        let b = self.lower_expr(bytes_expr)?;
        let ik = self.kind_of(idx_expr);
        let i = Self::wir_convert(self.lower_expr(idx_expr)?, ik, Kind::I64);

        let bin32 = |op, l, r| W::Binary { op, kind: WirKind::I32, lhs: Box::new(l), rhs: Box::new(r) };
        let bin64 = |op, l, r| W::Binary { op, kind: WirKind::I64, lhs: Box::new(l), rhs: Box::new(r) };

        let b_scratch = var_scratch("result", 0, Kind::I32);
        let i_scratch = var_scratch("result", 0, Kind::I64);

        let get_b = W::GetLocal(b_scratch.clone());
        let get_i = W::GetLocal(i_scratch.clone());

        let len_i64 = W::Convert {
            from: WirKind::I32,
            to: WirKind::I64,
            arg: Box::new(W::Load { ptr: Box::new(get_b.clone()), kind: WirKind::I32, offset: 0 }),
        };
        let i_i32 = W::Convert {
            from: WirKind::I64,
            to: WirKind::I32,
            arg: Box::new(get_i.clone()),
        };

        let bounds_proven = matches!((bytes_expr, idx_expr), (Expr::Var(bytes), Expr::Var(index))
            if self.elide_index_list.iter().any(|(proved_index, proved_root)| {
                proved_index == index && proved_root == bytes
            }));

        let mut seq = vec![
            N::SetLocal { local: b_scratch, value: b },
            N::SetLocal { local: i_scratch, value: i },
        ];
        if !bounds_proven {
            seq.push(N::If {
                cond: bin32(
                    BinOp::Or,
                    bin64(BinOp::Lt, get_i.clone(), W::ConstI64(0)),
                    bin64(BinOp::Ge, get_i.clone(), len_i64.clone()),
                ),
                then_: witchy_wir::wir_helpers::abort_nodes(
                    witchy_syntax::diag::DiagTemplate::BytesIndexOob,
                    get_i,
                    len_i64,
                    W::ConstI32(0),
                ),
                els: vec![],
                result: None,
            });
        }
        seq.push(N::Push(W::Convert {
            from: WirKind::I32,
            to: WirKind::I64,
            arg: Box::new(W::Load8U {
                ptr: Box::new(bin32(
                    BinOp::Add,
                    bin32(BinOp::Add, get_b, W::ConstI32(4)),
                    i_i32,
                )),
                offset: 0,
            }),
        }));
        Some(W::Seq(seq))
    }

    /// (RFC-0151) Direct byte write with range validation, bounds check, and `i32.store8`.
    pub(crate) fn lower_bytes_set_at(
        &mut self,
        bytes_expr: &Expr,
        idx_expr: &Expr,
        val_expr: &Expr,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use super::Kind;
        use witchy_wir::wir::{BinOp, Kind as WirKind, WirExpr as W, WirNode as N};
        let b = self.lower_expr(bytes_expr)?;
        let ik = self.kind_of(idx_expr);
        let vk = self.kind_of(val_expr);
        let i = Self::wir_convert(self.lower_expr(idx_expr)?, ik, Kind::I64);
        let v = Self::wir_convert(self.lower_expr(val_expr)?, vk, Kind::I64);

        let bin32 = |op, l, r| W::Binary { op, kind: WirKind::I32, lhs: Box::new(l), rhs: Box::new(r) };
        let bin64 = |op, l, r| W::Binary { op, kind: WirKind::I64, lhs: Box::new(l), rhs: Box::new(r) };

        let val_proven = match val_expr {
            Expr::Int(c) => *c >= 0 && *c <= 255,
            Expr::Var(vname) => {
                if let Expr::Var(rname) = bytes_expr {
                    self.active_linear_build_plan(rname)
                        .map(|p| p.range_is_byte_proven(vname))
                        .unwrap_or(false)
                } else {
                    false
                }
            }
            _ => false,
        };

        let b_scratch = var_scratch("result", 0, Kind::I32);
        let i_scratch = var_scratch("result", 0, Kind::I64);
        let v_scratch = var_scratch("result", 1, Kind::I64);

        let get_b = W::GetLocal(b_scratch.clone());
        let get_i = W::GetLocal(i_scratch.clone());
        let get_v = W::GetLocal(v_scratch.clone());

        let len_i64 = W::Convert {
            from: WirKind::I32,
            to: WirKind::I64,
            arg: Box::new(W::Load { ptr: Box::new(get_b.clone()), kind: WirKind::I32, offset: 0 }),
        };
        let i_i32 = W::Convert {
            from: WirKind::I64,
            to: WirKind::I32,
            arg: Box::new(get_i.clone()),
        };
        let v_i32 = W::Convert {
            from: WirKind::I64,
            to: WirKind::I32,
            arg: Box::new(get_v.clone()),
        };

        let bounds_proven = matches!((bytes_expr, idx_expr), (Expr::Var(bytes), Expr::Var(index))
            if self.elide_index_list.iter().any(|(proved_index, proved_root)| {
                proved_index == index && proved_root == bytes
            }));

        let mut seq = vec![
            N::SetLocal { local: b_scratch, value: b },
            N::SetLocal { local: i_scratch, value: i },
            N::SetLocal { local: v_scratch, value: v },
        ];

        if !val_proven {
            seq.push(N::If {
                cond: bin32(
                    BinOp::Or,
                    bin64(BinOp::Lt, get_v.clone(), W::ConstI64(0)),
                    bin64(BinOp::Gt, get_v, W::ConstI64(255)),
                ),
                then_: vec![N::Unreachable],
                els: vec![],
                result: None,
            });
        }

        if !bounds_proven {
            seq.push(N::If {
                cond: bin32(
                    BinOp::Or,
                    bin64(BinOp::Lt, get_i.clone(), W::ConstI64(0)),
                    bin64(BinOp::Ge, get_i.clone(), len_i64.clone()),
                ),
                then_: witchy_wir::wir_helpers::abort_nodes(
                    witchy_syntax::diag::DiagTemplate::BytesIndexOob,
                    get_i,
                    len_i64,
                    W::ConstI32(0),
                ),
                els: vec![],
                result: None,
            });
        }

        seq.push(N::Store8 {
            ptr: bin32(
                BinOp::Add,
                bin32(BinOp::Add, get_b.clone(), W::ConstI32(4)),
                i_i32,
            ),
            value: v_i32,
            offset: 0,
        });

        seq.push(N::Push(get_b));
        Some(W::Seq(seq))
    }

    /// (RFC-0151) Direct byte swap with bounds check and `i32.load8_u`/`i32.store8`.
    pub(crate) fn lower_bytes_swap(
        &mut self,
        bytes_expr: &Expr,
        i_expr: &Expr,
        j_expr: &Expr,
    ) -> Option<witchy_wir::wir::WirExpr> {
        use super::Kind;
        use witchy_wir::wir::{BinOp, Kind as WirKind, WirExpr as W, WirNode as N};
        let b = self.lower_expr(bytes_expr)?;
        let ik = self.kind_of(i_expr);
        let jk = self.kind_of(j_expr);
        let i = Self::wir_convert(self.lower_expr(i_expr)?, ik, Kind::I64);
        let j = Self::wir_convert(self.lower_expr(j_expr)?, jk, Kind::I64);

        let bin32 = |op, l, r| W::Binary { op, kind: WirKind::I32, lhs: Box::new(l), rhs: Box::new(r) };
        let bin64 = |op, l, r| W::Binary { op, kind: WirKind::I64, lhs: Box::new(l), rhs: Box::new(r) };

        let b_scratch = var_scratch("result", 0, Kind::I32);
        let i_scratch = var_scratch("result", 0, Kind::I64);
        let j_scratch = var_scratch("result", 1, Kind::I64);
        let t_scratch = var_scratch("result", 1, Kind::I32);

        let get_b = W::GetLocal(b_scratch.clone());
        let get_i = W::GetLocal(i_scratch.clone());
        let get_j = W::GetLocal(j_scratch.clone());
        let get_t = W::GetLocal(t_scratch.clone());

        let len_i64 = W::Convert {
            from: WirKind::I32,
            to: WirKind::I64,
            arg: Box::new(W::Load { ptr: Box::new(get_b.clone()), kind: WirKind::I32, offset: 0 }),
        };
        let i_i32 = W::Convert { from: WirKind::I64, to: WirKind::I32, arg: Box::new(get_i.clone()) };
        let j_i32 = W::Convert { from: WirKind::I64, to: WirKind::I32, arg: Box::new(get_j.clone()) };

        let ptr_i = bin32(BinOp::Add, bin32(BinOp::Add, get_b.clone(), W::ConstI32(4)), i_i32);
        let ptr_j = bin32(BinOp::Add, bin32(BinOp::Add, get_b.clone(), W::ConstI32(4)), j_i32);

        let seq = vec![
            N::SetLocal { local: b_scratch, value: b },
            N::SetLocal { local: i_scratch, value: i },
            N::SetLocal { local: j_scratch, value: j },
            N::If {
                cond: bin32(
                    BinOp::Or,
                    bin64(BinOp::Lt, get_i.clone(), W::ConstI64(0)),
                    bin64(BinOp::Ge, get_i.clone(), len_i64.clone()),
                ),
                then_: witchy_wir::wir_helpers::abort_nodes(
                    witchy_syntax::diag::DiagTemplate::BytesIndexOob,
                    get_i,
                    len_i64.clone(),
                    W::ConstI32(0),
                ),
                els: vec![],
                result: None,
            },
            N::If {
                cond: bin32(
                    BinOp::Or,
                    bin64(BinOp::Lt, get_j.clone(), W::ConstI64(0)),
                    bin64(BinOp::Ge, get_j.clone(), len_i64.clone()),
                ),
                then_: witchy_wir::wir_helpers::abort_nodes(
                    witchy_syntax::diag::DiagTemplate::BytesIndexOob,
                    get_j,
                    len_i64,
                    W::ConstI32(0),
                ),
                els: vec![],
                result: None,
            },
            N::SetLocal {
                local: t_scratch,
                value: W::Load8U { ptr: Box::new(ptr_i.clone()), offset: 0 },
            },
            N::Store8 {
                ptr: ptr_i,
                value: W::Load8U { ptr: Box::new(ptr_j.clone()), offset: 0 },
                offset: 0,
            },
            N::Store8 {
                ptr: ptr_j,
                value: get_t,
                offset: 0,
            },
            N::Push(get_b),
        ];
        Some(W::Seq(seq))
    }
}
