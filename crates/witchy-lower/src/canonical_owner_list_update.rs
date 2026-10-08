use super::*;

impl FunctionLowerer<'_> {
    pub(super) fn replace_consumed_owner_list(
        &mut self,
        source: ValueId,
        index: ValueId,
        replacement: ValueId,
        list_type: Type,
        site: &abort::Site,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let family = self.owner_list_family(source).cloned()
            .ok_or(CanonicalLoweringError::UnsupportedExpression("consuming owner list family"))?;
        if self.values[&index] != ValueType::Int
            || self.values[&replacement] != ValueType::Gc(family.owner_type)
        {
            return Err(CanonicalLoweringError::UnsupportedListElement(list_type));
        }
        self.diagnose_list_index(source, index, site)?;
        let index_size = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::NarrowSize { destination: index_size, value: index, exact: false });
        let length = self.owner_list_length(source, family.signature)?;
        let in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, index_size, length)?;
        self.push_guard(in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let value = self.clone_reference_token(replacement)?;
        let destination = self.fresh(ValueType::Gc(family.list_type))?;
        self.push_operation(Operation::ClosureOwner(OwnerOperation::ListSet {
            destination, list: source, index: index_size, value, family: family.signature,
        }));
        Ok(destination)
    }
}
