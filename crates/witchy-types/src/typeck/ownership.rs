//! Ownership transfer when an operation consumes an aggregate shell.

use super::{terr, Checker, Expr, Ty, TypeError};

impl Checker {
    /// Sum extraction and record reconstruction move selected fields out and
    /// retire the old shell after checking every discarded field's disposition.
    /// Reuse stored-type classification and ordinary transfer facts so neither
    /// a later expression nor a lazy fallback can use that owner again.
    pub(super) fn transfer_consuming_aggregate_operand(
        &mut self,
        expression: &Expr,
        ty: &Ty,
        context: &str,
    ) -> Result<(), TypeError> {
        let explicitly_moved = matches!(expression, Expr::Unary { op: super::UnOp::Move, .. });
        if let Some((root, _)) = self.affine_place(expression)
            && self.is_consumed_binding(&root) && !explicitly_moved
        {
            return terr(format!("{context} uses `{root}` after it was already transferred"));
        }
        self.reject_implicit_once_copy(expression, ty, context)?;
        // A checked `move` already performed this transition during inference.
        if !explicitly_moved { self.transfer_unique_value(expression, ty, context)?; }
        if !self.ty_carries_must_consume(ty) {
            return Ok(());
        }
        if let Some((root, projected)) = self.affine_place(expression) {
            let preserving_self_update = !projected
                && self.must_self_update_target.as_deref() == Some(root.as_str());
            if !preserving_self_update {
                self.reject_own_of_borrowed_must(&root, context)?;
            }
            if projected {
                return terr(format!(
                    "{context} cannot partially consume `{root}` because its aggregate carries a must-consume obligation; transfer the whole value to an `own` operation"
                ));
            }
            if !preserving_self_update {
                self.reject_implicit_must_copy(expression, context)?;
            }
            self.transfer_owned_argument_binding(&root);
        }
        Ok(())
    }
}
