//! Independently derive the acyclic encoding plan from checked source blocks.
use super::*;

impl PlanEmitter<'_> {
    pub(super) fn emit_forward(&mut self, order: &[crate::canonical_safewir::BlockId]) -> Result<(), CanonicalAdmissionError> {
        let positions: BTreeMap<_, _> = order.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        for _ in order.iter().skip(1) { self.plan.push(PlanToken::Block("Empty".into())); }
        for (position, id) in order.iter().enumerate() {
            if position > 0 { self.plan.push(PlanToken::End); }
            if !self.emitted.insert(*id) { return Err(CanonicalAdmissionError::UnsupportedPlan); }
            let block = self.block(*id)?;
            self.plan.extend(expected_block_operations_plan(self.source_function, block,
                self.layout, self.layouts, self.function_indices, self.signature_indices,
                self.imports, self.import_indices, self.static_data_offsets,
                &mut self.aggregates, self.owner_cleanup_function)?);
            let depth = |target, conditional| positions.get(&target)
                .and_then(|target| forward_cfg::depth(position, *target, conditional))
                .or_else(|| self.escape_label(target).and_then(|label| {
                    let extra = u32::try_from(order.len() - position - 1 + usize::from(conditional)).ok()?;
                    self.label_depth(label).checked_add(extra)
                }))
                .ok_or(CanonicalAdmissionError::UnsupportedPlan);
            match &block.terminator {
                Terminator::Branch { target, arguments } => {
                    let depth = depth(*target, false)?;
                    self.push_branch_arguments(*target, arguments)?;
                    self.plan.push(PlanToken::Br(depth));
                }
                Terminator::BranchIf { condition, then_target, then_arguments, else_target, else_arguments }
                    if let Some(then_cold) = forward_cfg::cold_arm(&self.doomed, *then_target, then_arguments.len(), *else_target, else_arguments.len()) =>
                {
                    let (cold, hot, hot_arguments) = if then_cold {
                        (*then_target, *else_target, else_arguments)
                    } else {
                        (*else_target, *then_target, then_arguments)
                    };
                    let cold_depth = depth(cold, false)?;
                    let hot_depth = depth(hot, false)?;
                    push_get(&mut self.plan, self.layout, *condition)?;
                    if !then_cold { self.plan.push(PlanToken::I32Eqz); }
                    self.plan.push(PlanToken::BrIf(cold_depth));
                    self.push_branch_arguments(hot, hot_arguments)?;
                    self.plan.push(PlanToken::Br(hot_depth));
                }
                Terminator::BranchIf { condition, then_target, then_arguments, else_target, else_arguments } => {
                    let then_depth = depth(*then_target, true)?;
                    let else_depth = depth(*else_target, true)?;
                    push_get(&mut self.plan, self.layout, *condition)?;
                    self.plan.push(PlanToken::If("Empty".into()));
                    self.push_branch_arguments(*then_target, then_arguments)?;
                    self.plan.push(PlanToken::Br(then_depth));
                    self.plan.push(PlanToken::Else);
                    self.push_branch_arguments(*else_target, else_arguments)?;
                    self.plan.push(PlanToken::Br(else_depth));
                    self.plan.push(PlanToken::End);
                }
                terminator => self.push_terminator(terminator)?,
            }
        }
        Ok(())
    }
}
