//! Constructor children use the instantiated stored-field contract recursively.

use super::*;

impl FunctionLowerer<'_> {
    pub(super) fn lower_checked_aggregate_payload(
        &mut self,
        arguments: &[Expr],
        shape: &ScalarAggregateShape,
        variant_index: usize,
    ) -> Result<Vec<ValueId>, CanonicalLoweringError> {
        let variant = shape.variants.get(variant_index)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("checked aggregate variant"))?;
        let end = variant.first_field.checked_add(variant.arity)
            .ok_or(CanonicalLoweringError::IdOverflow)?;
        let fields = shape.fields.get(variant.first_field..end)
            .filter(|fields| fields.len() == arguments.len())
            .ok_or(CanonicalLoweringError::UnsupportedExpression("checked aggregate payload arity"))?;
        arguments.iter().zip(fields).map(|(expression, field)| {
            self.lower_expr_for_storage(expression, Some(field))
        }).collect()
    }
}
