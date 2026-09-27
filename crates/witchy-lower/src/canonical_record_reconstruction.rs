//! Reconstruct a record by moving its selected stored fields, not copying an
//! affine shell. Field disposition comes from the checked recursive layout.

use super::*;

impl FunctionLowerer<'_> {
    pub(super) fn lower_consuming_record_reconstruction(
        &mut self,
        base: &Expr,
        base_type: &Type,
        result_type: &Type,
        result_shape: &ScalarAggregateShape,
        overrides: &[(usize, ValueId)],
    ) -> Result<ValueId, CanonicalLoweringError> {
        let base_shape = scalar_aggregate_shape(base_type, self.definitions)
            .filter(|shape| !shape.tagged && !shape.packed && shape.variants.len() == 1)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("consuming record source shape"))?;
        if result_shape.tagged || result_shape.variants.len() != 1
            || base_shape.fields != result_shape.fields
        {
            return Err(CanonicalLoweringError::UnsupportedExpression("consuming record field contract drift"));
        }
        let selected: Vec<bool> = (0..base_shape.fields.len())
            .map(|index| !overrides.iter().any(|(field, _)| *field == index)).collect();
        // Written overrides have already been evaluated. Take unchanged fields
        // only now, then retire this exact shell. Overwritten non-must fields
        // are released by the same checked drop/owned-token placement as any
        // other unused value; source checking forbids dropping a must field.
        let object = self.lower_expr(base)?;
        self.retire_consumed_aggregate_parameter(base, object);
        let old_fields = self.take_consuming_aggregate_fields(object, base_type, 0, &base_shape, &selected)?;
        let values = old_fields.into_iter().enumerate().map(|(index, original)| {
            overrides.iter().find(|(field, _)| *field == index).map(|(_, value)| *value)
                .or(original).ok_or(CanonicalLoweringError::UnsupportedExpression("missing reconstructed record field"))
        }).collect::<Result<Vec<_>, _>>()?;
        self.lower_aggregate_for_source(result_type, result_shape, 0, values)
    }
}
