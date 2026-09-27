//! Thread checked storage through value edges without changing return context.

use super::*;

impl FunctionLowerer<'_> {
    pub(super) fn lower_flow_value_expr(
        &mut self,
        expression: &Expr,
    ) -> Result<Option<ValueId>, CanonicalLoweringError> {
        self.lower_flow_value_expr_for_storage(expression, None)
    }

    /// `None` means the path terminated: it contributes no fabricated value
    /// or edge to a continuing join. Explicit returns keep function context.
    pub(super) fn lower_flow_value_expr_for_storage(
        &mut self,
        expression: &Expr,
        declared: Option<&Type>,
    ) -> Result<Option<ValueId>, CanonicalLoweringError> {
        let affine = |this: &Self| match declared {
            Some(ty) => Ok(witchy_canonical_source::storage::affine_storage_shape(ty, this.definitions).is_some()
                || StoragePlan::classify(ty, this.definitions, |_| Some(0)).requires_affine_owner_token()),
            None => this.affine_expression_result(expression),
        };
        match expression {
            Expr::Call { name, args } if name == "fail" => {
                self.lower_abort(expression, args)?;
                Ok(None)
            }
            Expr::If { cond, then_block, else_block: Some(else_block) } =>
                self.lower_if(cond, then_block, else_block, true, affine(self)?, declared),
            Expr::Match { scrutinee, arms } =>
                self.lower_match(scrutinee, arms, true, None, affine(self)?, declared),
            Expr::Block(block) => self.lower_block_flow_value_for_storage(block, declared),
            _ => self.lower_expr_for_storage(expression, declared).map(Some),
        }
    }

    pub(super) fn lower_block_flow_value(
        &mut self,
        block: &witchy_syntax::ast::Block,
    ) -> Result<Option<ValueId>, CanonicalLoweringError> {
        self.lower_block_flow_value_for_storage(block, None)
    }
}
