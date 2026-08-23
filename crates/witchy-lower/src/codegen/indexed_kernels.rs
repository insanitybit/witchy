//! RFC-0146 loop-scoped indexed-sequence lowering.
//!
//! The existing bounds-elision proof answers whether an access may omit its
//! trap guard. This module freezes the narrower physical contract that becomes
//! available when the same list root is stable for the whole loop: its header,
//! payload base, stride, and proven induction variables may live in locals.
//! Consumers ask for an address or length by typed root/index identity; they do
//! not recognize `list.at`, `list.set_at`, or benchmark source shapes.

use super::*;
use witchy_wir::wir::{BinOp as WirBinOp, Kind as WirKind, WirExpr as W, WirNode as N};

const SEQUENCE_POLICY_EXACT: u8 = 1;
const SEQUENCE_POLICY_GENERALIZED: u8 = 2;

/// Physical facts shared by all indexed operations in one stable loop.
#[derive(Clone, Debug)]
pub(super) struct SequenceAccessPlan {
    pub(super) owner_root: String,
    pub(super) payload_base: String,
    pub(super) length: String,
    pub(super) stride: i32,
    pub(super) element_kind: WirKind,
    pub(super) mutation_preserves_length: bool,
    pub(super) proven_index_domain: Vec<ProvenIndexDomain>,
    cursors: HashMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProvenIndexDomain {
    pub(super) index: String,
    pub(super) minimum_offset: i64,
    pub(super) maximum_offset: i64,
}

impl<'types> Codegen<'types> {
    fn note_sequence_backend_policy(&self, trigger: u8) {
        self.sequence_backend_policy_triggers
            .set(self.sequence_backend_policy_triggers.get() | trigger);
    }

    /// Single evidence-controlled selector for the module-level runtime policy.
    /// Exact and generalized consumption are recorded separately so repaired-
    /// master measurements can change this decision without touching lowering.
    pub(super) fn sequence_preserve_raw_requested(&self) -> bool {
        self.sequence_backend_policy_triggers.get() & SEQUENCE_POLICY_GENERALIZED != 0
    }

    pub(super) fn sequence_candidate_indices(&self, body: &Block) -> Vec<String> {
        let mut scan = DevirtScan::default();
        scan.walk_block(body);
        let mut indices: Vec<_> = scan
            .direct_indexed_accesses
            .keys()
            .map(|(index, _)| index.clone())
            .collect();
        indices.sort();
        indices.dedup();
        indices
    }

    /// Fully unroll a tiny literal range only when the already-lowered WIR body
    /// fits a fixed code-size budget. Larger or dynamic loops retain the landed
    /// four-lane loop with its scalar remainder guards.
    pub(super) fn small_constant_trip_count(
        &self,
        lo: &Expr,
        hi: &Expr,
        inclusive: bool,
        body: &witchy_wir::wir::WirSeq,
    ) -> Option<u32> {
        const MAX_TRIPS: i64 = 8;
        const WIR_NODE_BUDGET: usize = 48;
        let (Expr::Int(lo), Expr::Int(hi)) = (lo, hi) else {
            return None;
        };
        let trip_count = if inclusive {
            hi.checked_sub(*lo)?.checked_add(1)?
        } else {
            hi.checked_sub(*lo)?
        };
        if !(1..=MAX_TRIPS).contains(&trip_count)
            || body.len().checked_mul(trip_count as usize)? > WIR_NODE_BUDGET
        {
            return None;
        }
        Some(trip_count as u32)
    }

    /// Preserve the landed while-loop bounds proof while moving its result into
    /// the shared plan path. This is deliberately the same conservative proof:
    /// a known-nonnegative increasing lower index, an unchanged candidate list,
    /// and (when present) a decreasing upper index derived from that list.
    pub(super) fn while_bounds_elide_pairs(
        &self,
        cond: &Expr,
        body: &Block,
    ) -> Vec<(String, String)> {
        if !witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::BoundsElide) {
            return Vec::new();
        }
        let Expr::Binary { op: BinOp::Lt, lhs, rhs } = cond else {
            return Vec::new();
        };
        let Expr::Var(index) = lhs.as_ref() else {
            return Vec::new();
        };
        if !self.known_non_negative_vars.contains(index) {
            return Vec::new();
        }
        let candidate_lists: Vec<String> = match rhs.as_ref() {
            Expr::Call { name, args }
                if name == intrinsics::LIST_LENGTH
                    && matches!(args.as_slice(), [Expr::Var(_)]) =>
            {
                let Expr::Var(list) = &args[0] else { unreachable!() };
                vec![list.clone()]
            }
            Expr::Var(bound) => self
                .known_length_vars
                .get(bound)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect(),
            _ => Vec::new(),
        };

        let mut scan = DevirtScan::default();
        scan.walk_block(body);
        let mut pairs = Vec::new();
        for list in candidate_lists {
            let list_length_stable = !scan.let_bind.contains_key(&list)
                && !scan.other_bind.contains(&list)
                && !scan.length_changing_reassigned.contains(&list);
            if !list_length_stable {
                continue;
            }
            let mut assignments = 0;
            if increasing_induction(
                body,
                index,
                &mut assignments,
                &self.known_non_negative_vars,
            ) && assignments > 0
            {
                pairs.push((index.clone(), list.clone()));
                if let Expr::Var(upper) = rhs.as_ref() {
                    let mut upper_assignments = 0;
                    if decreasing_induction(body, upper, &mut upper_assignments)
                        && upper_assignments > 0
                    {
                        pairs.push((upper.clone(), list));
                    }
                }
            }
        }
        pairs
    }

    /// Install plans for bounds-proven `(index, list)` pairs while lowering one
    /// loop body. `initialize_cursors` is true for a `while`, whose induction
    /// locals already hold their entry values. Counted `for` loops synchronize
    /// their compiler-managed counter immediately before each body execution.
    pub(super) fn install_sequence_access_plans(
        &mut self,
        proven_pairs: &[(String, String)],
        eligible_indices: &[String],
        body: &Block,
        initialize_cursors: bool,
    ) -> (usize, witchy_wir::wir::WirSeq) {
        if eligible_indices.is_empty() {
            return (0, Vec::new());
        }

        let mut scan = DevirtScan::default();
        scan.walk_block(body);
        let mut grouped: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for (index, list) in scan
            .direct_indexed_accesses
            .keys()
            .filter(|(index, _)| eligible_indices.contains(index))
            .chain(proven_pairs.iter())
        {
            let indices = grouped.entry(list.clone()).or_default();
            if !indices.contains(index) {
                indices.push(index.clone());
            }
        }

        let mut setup = Vec::new();
        let before = self.sequence_access_plans.len();
        for (ordinal, (owner_root, mut indices)) in grouped.into_iter().enumerate() {
            indices.sort();
            // A read-only root is trivially stable. A root updated only by
            // same-length writes is also stable when the general ownership
            // analysis proved it confined and uniquely mutable: the existing
            // set-at path then stores directly and never replaces the pointer.
            let mutation_preserves_length = scan.reassigned.contains(&owner_root)
                && !scan.length_changing_reassigned.contains(&owner_root);
            let root_stable = !scan.reassigned.contains(&owner_root)
                || (mutation_preserves_length && self.inplace_push.contains(&owner_root));
            if !root_stable || scan.opaque_call_roots.contains(&owner_root) {
                continue;
            }

            let root_expr = Expr::Var(owner_root.clone());
            if self.gc_reference_list_layout_of_expr(&root_expr).is_some() {
                continue;
            }
            let element_kind = Self::wir_kind(self.list_elem_kind(&root_expr));
            let (data_offset, stride) = self
                .specialized_layout_of_expr(&root_expr)
                .and_then(|id| self.specialized_layouts.get(id))
                .and_then(|descriptor| match (descriptor.header(), descriptor.size()) {
                    (
                        HeaderLayout::PackedList { data_offset, .. },
                        LayoutSize::Dynamic { stride, .. },
                    ) => Some((data_offset as i32, stride as i32)),
                    _ => None,
                })
                .unwrap_or((4, 8));
            let id = format!("{}_{}_{}", self.next_label, before, ordinal);
            let payload_base = format!("__seq_base_{id}");
            let length = format!("__seq_len_{id}");
            self.locals.insert(payload_base.clone(), Kind::I32);
            self.locals.insert(length.clone(), Kind::I32);
            setup.push(N::SetLocal {
                local: payload_base.clone(),
                value: W::Binary {
                    op: WirBinOp::Add,
                    kind: WirKind::I32,
                    lhs: Box::new(W::GetLocal(owner_root.clone())),
                    rhs: Box::new(W::ConstI32(data_offset)),
                },
            });
            let all_accesses_proven_exact = indices.iter().all(|index| {
                proven_pairs.contains(&(index.clone(), owner_root.clone()))
                    && scan
                        .direct_indexed_accesses
                        .get(&(index.clone(), owner_root.clone()))
                        .is_none_or(|range| *range == (0, 0))
            });
            if initialize_cursors
                || !all_accesses_proven_exact
                || scan.length_reads.contains(&owner_root)
            {
                setup.push(N::SetLocal {
                    local: length.clone(),
                    value: W::Load {
                        ptr: Box::new(W::GetLocal(owner_root.clone())),
                        kind: WirKind::I32,
                        offset: 0,
                    },
                });
            }

            let mut cursors = HashMap::new();
            for (cursor_ordinal, index) in indices.iter().enumerate() {
                let cursor = format!("__seq_cursor_{id}_{cursor_ordinal}");
                self.locals.insert(cursor.clone(), Kind::I32);
                cursors.insert(index.clone(), cursor.clone());
                if initialize_cursors {
                    setup.push(N::SetLocal {
                        local: cursor,
                        value: Self::sequence_cursor_value(&payload_base, index, stride),
                    });
                }
            }
            self.sequence_access_plans.push(SequenceAccessPlan {
                owner_root: owner_root.clone(),
                payload_base,
                length,
                stride,
                element_kind,
                mutation_preserves_length,
                proven_index_domain: indices
                    .iter()
                    .filter(|index| proven_pairs.contains(&((*index).clone(), owner_root.clone())))
                    .cloned()
                    .map(|index| ProvenIndexDomain {
                        index,
                        minimum_offset: 0,
                        maximum_offset: 0,
                    })
                    .collect(),
                cursors,
            });
        }
        (self.sequence_access_plans.len() - before, setup)
    }

    pub(super) fn pop_sequence_access_plans(&mut self, count: usize) {
        let keep = self.sequence_access_plans.len().saturating_sub(count);
        self.sequence_access_plans.truncate(keep);
    }

    /// Refine the exact-index domain for a counted range. For
    /// `lo..list.length(xs)-upper_slack`, every affine access `i + offset`
    /// within `-lo..=upper_slack` is covered by the loop's single proof.
    pub(super) fn refine_counted_sequence_domain(
        &mut self,
        index: &str,
        list: &str,
        lo: &Expr,
        hi: &Expr,
    ) {
        let Expr::Int(lower) = lo else { return };
        let upper_slack = match hi {
            Expr::Call { name, args }
                if name == intrinsics::LIST_LENGTH
                    && matches!(args.as_slice(), [Expr::Var(root)] if root == list) => 0,
            Expr::Binary { op: BinOp::Sub, lhs, rhs }
                if matches!(lhs.as_ref(), Expr::Call { name, args }
                    if name == intrinsics::LIST_LENGTH
                        && matches!(args.as_slice(), [Expr::Var(root)] if root == list)) =>
            {
                let Expr::Int(slack) = rhs.as_ref() else { return };
                if *slack < 0 { return }
                *slack
            }
            _ => return,
        };
        if *lower < 0 {
            return;
        }
        for plan in self.sequence_access_plans.iter_mut().rev() {
            if plan.owner_root != list {
                continue;
            }
            if let Some(domain) = plan
                .proven_index_domain
                .iter_mut()
                .find(|domain| domain.index == index)
            {
                domain.minimum_offset = -*lower;
                domain.maximum_offset = upper_slack;
                return;
            }
        }
    }

    /// For a single straight-line indexed statement, one guard on the widest
    /// affine lane dominates every access in that statement. This deliberately
    /// declines multi-statement bodies: moving a later trap ahead of an earlier
    /// visible store would change trap ordering.
    pub(super) fn coalesce_counted_sequence_guards(
        &mut self,
        index: &str,
        lo: &Expr,
        body: &Block,
    ) -> witchy_wir::wir::WirSeq {
        let Expr::Int(lower) = lo else { return Vec::new() };
        if *lower < 0 || body.stmts.len() != 1 {
            return Vec::new();
        }
        let mut scan = DevirtScan::default();
        scan.walk_block(body);
        let mut guards = Vec::new();
        for plan in self.sequence_access_plans.iter_mut() {
            let Some(&(minimum_offset, maximum_offset)) = scan
                .direct_indexed_accesses
                .get(&(index.to_string(), plan.owner_root.clone()))
            else {
                continue;
            };
            if minimum_offset < -*lower
                || i32::try_from(minimum_offset.checked_mul(i64::from(plan.stride)).unwrap_or(i64::MAX)).is_err()
                || i32::try_from(maximum_offset.checked_mul(i64::from(plan.stride)).unwrap_or(i64::MAX)).is_err()
            {
                continue;
            }
            if plan.proven_index_domain.iter().any(|domain| {
                domain.index == index
                    && domain.minimum_offset <= minimum_offset
                    && domain.maximum_offset >= maximum_offset
            }) {
                continue;
            }
            let checked_index = if maximum_offset == 0 {
                W::GetLocal(index.to_string())
            } else {
                W::Binary {
                    op: WirBinOp::Add,
                    kind: WirKind::I64,
                    lhs: Box::new(W::GetLocal(index.to_string())),
                    rhs: Box::new(W::ConstI64(maximum_offset)),
                }
            };
            guards.push(N::If {
                cond: W::Binary {
                    op: WirBinOp::GeU,
                    kind: WirKind::I64,
                    lhs: Box::new(checked_index.clone()),
                    rhs: Box::new(W::Convert {
                        from: WirKind::I32,
                        to: WirKind::I64,
                        arg: Box::new(W::GetLocal(plan.length.clone())),
                    }),
                },
                then_: vec![N::Drop(W::Call {
                    func: "list_at".into(),
                    args: vec![W::GetLocal(plan.owner_root.clone()), checked_index],
                })],
                els: vec![],
                result: None,
            });
            if let Some(domain) = plan
                .proven_index_domain
                .iter_mut()
                .find(|domain| domain.index == index)
            {
                domain.minimum_offset = domain.minimum_offset.min(minimum_offset);
                domain.maximum_offset = domain.maximum_offset.max(maximum_offset);
            } else {
                plan.proven_index_domain.push(ProvenIndexDomain {
                    index: index.to_string(),
                    minimum_offset,
                    maximum_offset,
                });
            }
        }
        if !guards.is_empty() {
            self.note_sequence_backend_policy(SEQUENCE_POLICY_GENERALIZED);
        }
        guards
    }

    fn sequence_cursor_value(payload_base: &str, index: &str, stride: i32) -> W {
        W::Binary {
            op: WirBinOp::Add,
            kind: WirKind::I32,
            lhs: Box::new(W::GetLocal(payload_base.to_string())),
            rhs: Box::new(W::Binary {
                op: WirBinOp::Mul,
                kind: WirKind::I32,
                lhs: Box::new(W::Convert {
                    from: WirKind::I64,
                    to: WirKind::I32,
                    arg: Box::new(W::GetLocal(index.to_string())),
                }),
                rhs: Box::new(W::ConstI32(stride)),
            }),
        }
    }

    /// Seed compiler-managed counted-loop cursors from the loop counter. The
    /// cursor is then advanced by stride after each logical iteration instead
    /// of rebuilding `base + index * stride` for every unrolled lane.
    pub(super) fn initialize_sequence_cursors_from(
        &self,
        index: &str,
        source: &str,
    ) -> witchy_wir::wir::WirSeq {
        self.sequence_access_plans
            .iter()
            .filter_map(|plan| {
                plan.cursors.get(index).map(|cursor| N::SetLocal {
                    local: cursor.clone(),
                    value: Self::sequence_cursor_value(&plan.payload_base, source, plan.stride),
                })
            })
            .collect()
    }

    pub(super) fn advance_sequence_cursors(
        &self,
        index: &str,
        steps: i32,
    ) -> witchy_wir::wir::WirSeq {
        self.sequence_access_plans
            .iter()
            .filter_map(|plan| {
                let cursor = plan.cursors.get(index)?;
                let byte_step = plan.stride.checked_mul(steps)?;
                Some(N::SetLocal {
                    local: cursor.clone(),
                    value: W::Binary {
                        op: WirBinOp::Add,
                        kind: WirKind::I32,
                        lhs: Box::new(W::GetLocal(cursor.clone())),
                        rhs: Box::new(W::ConstI32(byte_step)),
                    },
                })
            })
            .collect()
    }

    /// Direct cursor address for a bounds-proven access. Repeated accesses with
    /// the same `(root, index)` therefore share the hoisted header and address.
    pub(super) fn sequence_element_address(
        &self,
        list: &str,
        index: &Expr,
        element_kind: WirKind,
    ) -> Option<W> {
        let address = self.sequence_element_address_impl(list, index, element_kind, true);
        if address.is_some() {
            self.note_sequence_backend_policy(SEQUENCE_POLICY_EXACT);
        }
        address
    }

    /// Address available from a stable physical plan even when this particular
    /// access still needs its normal bounds trap. The proof and the address are
    /// intentionally separate facts.
    pub(super) fn planned_sequence_element_address(
        &self,
        list: &str,
        index: &Expr,
        element_kind: WirKind,
    ) -> Option<W> {
        let address = self.sequence_element_address_impl(list, index, element_kind, false);
        if address.is_some() {
            self.note_sequence_backend_policy(SEQUENCE_POLICY_GENERALIZED);
        }
        address
    }

    fn sequence_element_address_impl(
        &self,
        list: &str,
        index: &Expr,
        element_kind: WirKind,
        require_proof: bool,
    ) -> Option<W> {
        let (index, offset) = affine_index(index)?;
        let plan = self.sequence_access_plans
            .iter()
            .rev()
            .find(|plan| {
                plan.owner_root == list
                    && plan.element_kind == element_kind
                    && (!require_proof || plan.proven_index_domain.iter().any(|domain| {
                        domain.index == index
                            && (domain.minimum_offset..=domain.maximum_offset).contains(&offset)
                    }))
            })?;
        let cursor = plan.cursors.get(index)?.clone();
        if offset == 0 {
            return Some(W::GetLocal(cursor));
        }
        let byte_offset = i32::try_from(offset.checked_mul(i64::from(plan.stride))?).ok()?;
        Some(W::Binary {
            op: WirBinOp::Add,
            kind: WirKind::I32,
            lhs: Box::new(W::GetLocal(cursor)),
            rhs: Box::new(W::ConstI32(byte_offset)),
        })
    }

    /// Hoisted list length for calls inside the plan lifetime.
    pub(super) fn sequence_length(&self, list: &str) -> Option<W> {
        let length = self.sequence_access_plans
            .iter()
            .rev()
            .find(|plan| plan.owner_root == list)
            .map(|plan| W::GetLocal(plan.length.clone()));
        if length.is_some() {
            self.note_sequence_backend_policy(SEQUENCE_POLICY_EXACT);
        }
        length
    }

    /// Make a direct scalar store available to the next matching load in this
    /// basic-block lowering stream. The store's ordinary bounds/capacity path
    /// has already completed; consuming this cache cannot move or remove its
    /// trap. Clearing at every subsequent source statement makes this a narrow
    /// local forwarding rule rather than alias analysis.
    pub(super) fn remember_sequence_store(
        &mut self,
        list: &str,
        index: &Expr,
    ) -> Option<N> {
        let (index_root, offset) = affine_index(index)?;
        if self
            .planned_sequence_element_address(list, index, WirKind::I64)
            .is_none()
        {
            return None;
        }
        let local = format!("__seq_forward_{}_{}", self.next_label, self.sequence_forwarded_values.len());
        self.locals.insert(local.clone(), Kind::I64);
        self.sequence_forwarded_values.insert(
            (list.to_string(), index_root.to_string(), offset),
            local.clone(),
        );
        Some(N::SetLocal {
            local,
            value: W::GetLocal("__witchy_set_val".into()),
        })
    }

    pub(super) fn next_statement_reads_sequence_value(
        &self,
        statement: Option<&Stmt>,
        list: &str,
        index: &Expr,
    ) -> bool {
        let Some((index_root, offset)) = affine_index(index) else { return false };
        let expression = match statement {
            Some(Stmt::Let { value, .. }) | Some(Stmt::Assign { value, .. }) => value,
            Some(Stmt::Expr(Expr::If { cond, .. })) => cond.as_ref(),
            Some(Stmt::Expr(value)) => value,
            _ => return false,
        };
        pure_expr_reads_address(expression, list, index_root, offset).unwrap_or(false)
    }

    pub(super) fn take_forwarded_sequence_value(
        &mut self,
        list: &str,
        index: &Expr,
        element_kind: WirKind,
    ) -> Option<W> {
        if element_kind != WirKind::I64 {
            return None;
        }
        let (index_root, offset) = affine_index(index)?;
        let local = self.sequence_forwarded_values.remove(&(
            list.to_string(),
            index_root.to_string(),
            offset,
        ))?;
        self.note_sequence_backend_policy(SEQUENCE_POLICY_GENERALIZED);
        Some(W::FromSlot(
            Box::new(W::GetLocal(local)),
            element_kind,
        ))
    }

    pub(super) fn clear_forwarded_sequence_values(&mut self) {
        self.sequence_forwarded_values.clear();
    }

    /// Keep cursor locals synchronized with source induction-variable writes in
    /// `while` bodies. The source assignment has already executed when these
    /// nodes run, so recomputing from the hoisted base preserves arbitrary proven
    /// nonnegative steps without accumulating arithmetic drift.
    pub(super) fn synchronize_sequence_cursors(
        &self,
        assigned: &str,
        value: &Expr,
    ) -> witchy_wir::wir::WirSeq {
        self.sequence_access_plans
            .iter()
            .filter(|plan| plan.mutation_preserves_length || assigned != plan.owner_root)
            .filter_map(|plan| {
                let cursor = plan.cursors.get(assigned)?;
                let incremental = match value {
                    Expr::Binary { op: BinOp::Add, lhs, rhs }
                        if matches!(lhs.as_ref(), Expr::Var(index) if index == assigned) =>
                    {
                        match rhs.as_ref() {
                            Expr::Int(step) => i32::try_from(
                                step.checked_mul(i64::from(plan.stride))?,
                            )
                            .ok()
                            .map(W::ConstI32),
                            Expr::Var(step) => Some(W::Binary {
                                op: WirBinOp::Mul,
                                kind: WirKind::I32,
                                lhs: Box::new(W::Convert {
                                    from: WirKind::I64,
                                    to: WirKind::I32,
                                    arg: Box::new(W::GetLocal(step.clone())),
                                }),
                                rhs: Box::new(W::ConstI32(plan.stride)),
                            }),
                            _ => None,
                        }
                    }
                    Expr::Binary { op: BinOp::Sub, lhs, rhs }
                        if matches!(lhs.as_ref(), Expr::Var(index) if index == assigned) =>
                    {
                        match rhs.as_ref() {
                            Expr::Int(step) => i32::try_from(
                                step.checked_neg()?.checked_mul(i64::from(plan.stride))?,
                            )
                            .ok()
                            .map(W::ConstI32),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                Some(N::SetLocal {
                    local: cursor.clone(),
                    value: if let Some(delta) = incremental {
                        W::Binary {
                            op: WirBinOp::Add,
                            kind: WirKind::I32,
                            lhs: Box::new(W::GetLocal(cursor.clone())),
                            rhs: Box::new(delta),
                        }
                    } else {
                        Self::sequence_cursor_value(&plan.payload_base, assigned, plan.stride)
                    },
                })
            })
            .collect()
    }
}

fn affine_index(expr: &Expr) -> Option<(&str, i64)> {
    match expr {
        Expr::Var(index) => Some((index, 0)),
        Expr::Binary { op: BinOp::Add, lhs, rhs } => match (lhs.as_ref(), rhs.as_ref()) {
            (Expr::Var(index), Expr::Int(offset))
            | (Expr::Int(offset), Expr::Var(index)) => Some((index, *offset)),
            _ => None,
        },
        Expr::Binary { op: BinOp::Sub, lhs, rhs } => match (lhs.as_ref(), rhs.as_ref()) {
            (Expr::Var(index), Expr::Int(offset)) => Some((index, offset.checked_neg()?)),
            _ => None,
        },
        _ => None,
    }
}

fn pure_expr_reads_address(
    expr: &Expr,
    list: &str,
    index: &str,
    offset: i64,
) -> Option<bool> {
    match expr {
        Expr::Call { name, args }
            if name == intrinsics::LIST_AT && args.len() == 2 =>
        {
            Some(matches!(&args[0], Expr::Var(root) if root == list)
                && affine_index(&args[1]) == Some((index, offset)))
        }
        Expr::Unary { expr, .. } | Expr::As { expr, .. } => {
            pure_expr_reads_address(expr, list, index, offset)
        }
        Expr::Binary { lhs, rhs, .. } => {
            match pure_expr_reads_address(lhs, list, index, offset)? {
                true => Some(true),
                false => pure_expr_reads_address(rhs, list, index, offset),
            }
        }
        Expr::Int(_) | Expr::Float(_) | Expr::Duration(_) | Expr::Str(_) | Expr::Bool(_)
        | Expr::Var(_) => Some(false),
        _ => None,
    }
}

fn increasing_induction<S: std::hash::BuildHasher>(
    block: &Block,
    index: &str,
    assignments: &mut usize,
    known_nonnegative: &std::collections::HashSet<String, S>,
) -> bool {
    for statement in &block.stmts {
        match statement {
            Stmt::Assign { name, value } if name == index => {
                *assignments += 1;
                let Expr::Binary { op: BinOp::Add, lhs, rhs } = value else {
                    return false;
                };
                let step_is_nonnegative = match rhs.as_ref() {
                    Expr::Int(step) => *step >= 0,
                    Expr::Var(step) => known_nonnegative.contains(step),
                    _ => false,
                };
                if !matches!(lhs.as_ref(), Expr::Var(var) if var == index)
                    || !step_is_nonnegative {
                    return false;
                }
            }
            Stmt::Let { name, .. } if name == index => return false,
            Stmt::Expr(Expr::While { body, .. }) => {
                if !increasing_induction(body, index, assignments, known_nonnegative) {
                    return false;
                }
            }
            Stmt::Expr(Expr::If { then_block, else_block, .. }) => {
                if !increasing_induction(then_block, index, assignments, known_nonnegative)
                    || else_block.as_ref().is_some_and(|block| {
                        !increasing_induction(block, index, assignments, known_nonnegative)
                    })
                {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

fn decreasing_induction(block: &Block, index: &str, assignments: &mut usize) -> bool {
    for statement in &block.stmts {
        match statement {
            Stmt::Assign { name, value } if name == index => {
                *assignments += 1;
                if !matches!(value,
                    Expr::Binary { op: BinOp::Sub, lhs, rhs }
                        if matches!(lhs.as_ref(), Expr::Var(var) if var == index)
                            && matches!(rhs.as_ref(), Expr::Int(step) if *step >= 0))
                {
                    return false;
                }
            }
            Stmt::Let { name, .. } if name == index => return false,
            Stmt::Expr(Expr::While { body, .. }) => {
                if !decreasing_induction(body, index, assignments) {
                    return false;
                }
            }
            Stmt::Expr(Expr::If { then_block, else_block, .. }) => {
                if !decreasing_induction(then_block, index, assignments)
                    || else_block
                        .as_ref()
                        .is_some_and(|block| !decreasing_induction(block, index, assignments))
                {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::SequenceAccessPlan;
    use foldhash::{HashMap, HashMapExt as _};
    use witchy_wir::wir::Kind;

    #[test]
    fn sequence_access_plan_contract_names_every_validity_fact() {
        let mut cursors = HashMap::new();
        cursors.insert("i".into(), "cursor".into());
        let plan = SequenceAccessPlan {
            owner_root: "xs".into(),
            payload_base: "base".into(),
            length: "len".into(),
            stride: 8,
            element_kind: Kind::I64,
            mutation_preserves_length: true,
            proven_index_domain: vec![super::ProvenIndexDomain {
                index: "i".into(),
                minimum_offset: 0,
                maximum_offset: 0,
            }],
            cursors,
        };
        assert_eq!(plan.owner_root, "xs");
        assert_eq!(plan.payload_base, "base");
        assert_eq!(plan.length, "len");
        assert_eq!(plan.stride, 8);
        assert_eq!(plan.element_kind, Kind::I64);
        assert!(plan.mutation_preserves_length);
        assert_eq!(plan.proven_index_domain[0].index, "i");
    }
}
