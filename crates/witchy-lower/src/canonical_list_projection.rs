use super::*;

impl FunctionLowerer<'_> {
    pub(super) fn lower_match_list_projection(
        &mut self,
        expression: &Expr,
        base: &Expr,
        index: &Expr,
    ) -> Result<Option<ValueId>, CanonicalLoweringError> {
        let Expr::Match { scrutinee, arms } = base else { return Ok(None) };
        if !matches!(index, Expr::Int(0)) || arms.is_empty()
            || !arms.iter().all(|arm| arm.guard.is_none()
                && matches!(&arm.body, Expr::List(items) if items.len() <= 1))
            || !arms.iter().any(|arm| matches!(&arm.body, Expr::List(items) if items.len() == 1))
        {
            return Ok(None);
        }
        let logical_type = self.expression_type(base)?;
        if matches!(StoragePlan::classify(&logical_type, self.definitions, |_| Some(0)), StoragePlan::MoveOnlyList(_)) {
            return Ok(None);
        }
        let result_type = self.expression_storage_type(expression)?;
        let site = self.diagnostic_site(expression)?;
        self.lower_match_with_list_projection(
            scrutinee, arms, true, None, self.affine_expression_result(expression)?, Some(&result_type), Some(&site),
        )
    }

    pub(super) fn lower_singleton_projection_arm(
        &mut self,
        expression: &Expr,
        declared: Option<&Type>,
        site: &abort::Site,
    ) -> Result<Option<ValueId>, CanonicalLoweringError> {
        let Expr::List(items) = expression else {
            return Err(CanonicalLoweringError::UnsupportedExpression("projected match list arm"));
        };
        match items.as_slice() {
            [] => {
                let index = self.push_const_int(0)?;
                let length = self.push_const_size(0)?;
                let import = self.host_imports.bounds.ok_or_else(|| CanonicalLoweringError::InvalidAuthenticatedAbiCatalog("bounds import missing".into()))?;
                self.abort_index_at(index, length, site, import)?;
                Ok(None)
            }
            [value] => self.lower_flow_value_expr_for_storage(value, declared),
            _ => Err(CanonicalLoweringError::UnsupportedExpression("projected match list arity")),
        }
    }
}
