//! Ownership transfer at consuming sum operations, before their control-flow split.

use super::{terr, Checker, Expr, Ty, TypeError};

impl Checker {
    /// `?` and `??` move an affine wrapper's payload out and retire the wrapper.
    /// Reuse stored-type classification and ordinary transfer facts so neither
    /// a later expression nor a lazy fallback can use that owner again.
    pub(super) fn transfer_consuming_sum_operand(
        &mut self,
        expression: &Expr,
        ty: &Ty,
        context: &str,
    ) -> Result<(), TypeError> {
        self.reject_implicit_once_copy(expression, ty, context)?;
        self.transfer_unique_value(expression, ty, context)?;
        if !self.ty_carries_must_consume(ty) {
            return Ok(());
        }
        if let Some((root, projected)) = self.affine_place(expression) {
            self.reject_own_of_borrowed_must(&root, context)?;
            if projected {
                return terr(format!(
                    "{context} cannot partially consume `{root}` because its aggregate carries a must-consume obligation; transfer the whole value to an `own` operation"
                ));
            }
            self.reject_implicit_must_copy(expression, context)?;
            self.transfer_owned_argument_binding(&root);
        }
        Ok(())
    }
}
