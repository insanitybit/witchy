//! Keep normal preparation distinct from checked aggregate representation.

use super::*;

impl FunctionLowerer<'_> {
    pub(super) fn call_argument_storage_type(
        &self,
        expression: &Expr,
        value: ValueId,
        declared: Option<&Type>,
    ) -> Result<Type, CanonicalLoweringError> {
        let original = self.expression_storage_type(expression)?;
        let actual = self.values[&value];
        if self.field_value_type(&original)? == actual {
            // In particular, requesting root uniqueness does not authenticate
            // unique origin. Ordinary normal-mode inputs still need copying.
            return Ok(original);
        }
        // Only a successfully checked ascription may have changed the physical
        // aggregate layout. Do not clone that result as its erased old layout.
        let declared = declared.ok_or(CanonicalLoweringError::UnsupportedExpression(
            "call argument storage lacks a checked declaration",
        ))?;
        if self.field_value_type(declared)? != actual {
            return Err(CanonicalLoweringError::UnsupportedExpression("call argument storage drift"));
        }
        Ok(declared.clone())
    }
}
