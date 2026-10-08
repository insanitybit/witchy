//! Shared checked binding lanes for pattern success and guard fallthrough.
use super::*;

impl FunctionLowerer<'_> {
    pub(super) fn join_match_binding_paths(
        &mut self,
        successes: Vec<(BlockId, HashMap<String, ValueId>)>,
        names: Vec<String>,
        mut continuing_facts: Vec<FactSnapshot>,
        facts: FactSnapshot,
    ) -> Result<Vec<(String, ValueId)>, CanonicalLoweringError> {
        self.restore_snapshot(facts.clone());
        for arm_facts in continuing_facts.iter().cloned() {
            self.absorb_facts(arm_facts);
        }
        let edges = successes.iter().map(|(block, bindings)| (*block, bindings)).collect::<Vec<_>>();
        let plans = self.plan_binding_lanes(&edges, &names, &mut continuing_facts)?;
        let mut parameter_types = Vec::new();
        for plan in &plans {
            parameter_types.push(self.values[&plan.values[0]]);
            if plan.has_lanes() {
                parameter_types.extend([ValueType::Size, ValueType::Size]);
            }
        }
        let join = self.new_block(&parameter_types)?;
        let parameters = self.blocks[join.get() as usize].parameters.clone();
        let mut bindings = Vec::new();
        let mut parameter_index = 0;
        for plan in &plans {
            let parameter = parameters[parameter_index].value;
            if let Some(phi) = plan.phi {
                self.blocks[join.get() as usize].parameters[parameter_index].ownership = Some(phi);
            }
            if plan.all_tokens {
                self.token_join_parameters.insert(parameter);
            }
            bindings.push((plan.name.clone(), parameter));
            parameter_index += 1 + if plan.has_lanes() { 2 } else { 0 };
        }
        for (edge, (block, _)) in successes.iter().enumerate() {
            let mut arguments = Vec::new();
            for plan in &plans {
                arguments.push(plan.values[edge]);
                if let (true, Some(extents)) = (plan.has_lanes(), &plan.extents) {
                    arguments.extend([extents[edge].0, extents[edge].1]);
                }
            }
            self.graph_version += 1;
            self.place_terminator(*block, Terminator::Branch { target: join, arguments })?;
        }
        self.restore_facts(facts, join);
        self.current = join;
        parameter_index = 0;
        for plan in &plans {
            if plan.has_lanes() {
                let value = parameters[parameter_index].value;
                let count = parameters[parameter_index + 1].value;
                let capacity = parameters[parameter_index + 2].value;
                if let Some(family) = plan.family {
                    self.record_joined_dynamic_extent(value, count, capacity, family);
                    self.guard_joined_dynamic_extent(value, count, capacity, family)?;
                }
            }
            parameter_index += 1 + if plan.has_lanes() { 2 } else { 0 };
        }
        Ok(bindings)
    }
}
