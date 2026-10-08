use super::*;

impl FunctionLowerer<'_> {
    pub(super) fn lower_checked_receive_preparation(&mut self, expression: &Expr, arguments: &[Expr])
        -> Result<Option<ValueId>, CanonicalLoweringError>
    {
        let Some(preparation) = self.canonical_source.checked_receive_preparation(expression) else { return Ok(None) };
        let endpoint_type = preparation.endpoint_type().clone();
        let [Expr::Block(block)] = arguments else {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked receive preparation operand"));
        };
        let Some(Stmt::Expr(Expr::Call { args, .. })) = block.stmts.last() else {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked receive preparation call"));
        };
        let [endpoint] = args.as_slice() else {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked receive preparation arity"));
        };
        let shape = scalar_aggregate_shape(&endpoint_type, self.definitions)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("checked receive endpoint layout"))?;
        let [variant] = &*shape.variants else {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked receive endpoint variants"));
        };
        if variant.arity != 1 { return Err(CanonicalLoweringError::UnsupportedExpression("checked receive endpoint fields")); }
        let channel_type = shape.fields[variant.first_field].clone();
        let channel_shape = scalar_aggregate_shape(&channel_type, self.definitions)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("checked receive channel layout"))?;
        let [channel_variant] = &*channel_shape.variants else {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked receive channel variants"));
        };
        if channel_variant.arity != 1 || channel_shape.fields[channel_variant.first_field] != Type::Named("Int".into(), Vec::new()) {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked receive channel field"));
        }
        let endpoint = self.lower_expr(endpoint)?;
        let channel_value_type = self.field_value_type(&channel_type)?;
        let channel = self.load_source_aggregate_field(endpoint, &endpoint_type, 0, 0, variant.first_field, channel_value_type)?;
        self.load_source_aggregate_field(channel, &channel_type, 0, 0, channel_variant.first_field, ValueType::Int).map(Some)
    }
}
