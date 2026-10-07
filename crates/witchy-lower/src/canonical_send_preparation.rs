use super::*;

impl FunctionLowerer<'_> {
    pub(super) fn lower_checked_send_preparation(&mut self, expression: &Expr, arguments: &[Expr])
        -> Result<Option<ValueId>, CanonicalLoweringError>
    {
        let Some(preparation) = self.canonical_source.checked_send_preparation(expression) else {
            return Ok(None);
        };
        let endpoint_type = preparation.endpoint_type().clone();
        let payload_type = preparation.payload_type().clone();
        let [Expr::Block(block)] = arguments else {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked send preparation operand"));
        };
        let Some(Stmt::Expr(Expr::Call { args, .. })) = block.stmts.last() else {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked send preparation call"));
        };
        let [endpoint, payload] = args.as_slice() else {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked send preparation arity"));
        };
        let result_type = self.expression_type(expression)?;
        let Type::Tuple(result_fields) = &result_type else {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked send preparation result"));
        };
        let [integer_type, message_type] = result_fields.as_slice() else {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked send preparation result arity"));
        };
        if integer_type != &Type::Named("Int".into(), Vec::new()) {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked send preparation channel id"));
        }
        let endpoint_shape = scalar_aggregate_shape(&endpoint_type, self.definitions)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("checked send endpoint layout"))?;
        let [sender_variant] = &*endpoint_shape.variants else {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked send endpoint variants"));
        };
        if sender_variant.arity != 1 {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked send endpoint fields"));
        }
        let channel_type = endpoint_shape.fields[sender_variant.first_field].clone();
        let channel_shape = scalar_aggregate_shape(&channel_type, self.definitions)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("checked channel id layout"))?;
        let [channel_variant] = &*channel_shape.variants else {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked channel id variants"));
        };
        if channel_variant.arity != 1 || channel_shape.fields[channel_variant.first_field] != *integer_type {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked channel id field"));
        }
        let message_shape = scalar_aggregate_shape(message_type, self.definitions)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("checked message layout"))?;
        let variants = message_shape.variants.iter().enumerate().filter(|(_, variant)| {
            variant.arity == 1 && message_shape.fields[variant.first_field] == payload_type
        }).map(|(index, _)| index).collect::<Vec<_>>();
        let [message_variant] = variants.as_slice() else {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked message payload identity"));
        };
        let sender = self.lower_expr(endpoint)?;
        let message = self.lower_expr(payload)?;
        let channel_value_type = self.field_value_type(&channel_type)?;
        let channel = self.load_source_aggregate_field(sender, &endpoint_type, 0, 0,
            sender_variant.first_field, channel_value_type)?;
        let channel_id = self.load_source_aggregate_field(channel, &channel_type, 0, 0,
            channel_variant.first_field, ValueType::Int)?;
        let message = self.lower_aggregate_for_source(message_type, &message_shape, *message_variant, vec![message])?;
        let result_shape = scalar_aggregate_shape(&result_type, self.definitions)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("checked send result layout"))?;
        self.lower_aggregate_for_source(&result_type, &result_shape, 0, vec![channel_id, message]).map(Some)
    }
}
