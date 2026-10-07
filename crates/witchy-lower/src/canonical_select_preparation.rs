use super::*;

impl FunctionLowerer<'_> {
    pub(super) fn lower_checked_select_preparation(&mut self, expression: &Expr, arguments: &[Expr])
        -> Result<Option<ValueId>, CanonicalLoweringError>
    {
        let Some(preparation) = self.canonical_source.checked_select_preparation(expression) else {
            return Ok(None);
        };
        let endpoint_types = preparation.endpoint_types().clone();
        let [Expr::Block(block)] = arguments else {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked select preparation operand"));
        };
        let Some(Stmt::Expr(Expr::Call { args, .. })) = block.stmts.last() else {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked select preparation call"));
        };
        if args.len() != 2 {
            return Err(CanonicalLoweringError::UnsupportedExpression("checked select preparation arity"));
        }
        let mut channels = Vec::new();
        for (endpoint, endpoint_type) in args.iter().zip(&endpoint_types) {
            let shape = scalar_aggregate_shape(endpoint_type, self.definitions)
                .ok_or(CanonicalLoweringError::UnsupportedExpression("checked select endpoint layout"))?;
            let [variant] = &*shape.variants else {
                return Err(CanonicalLoweringError::UnsupportedExpression("checked select endpoint variants"));
            };
            if variant.arity != 1 {
                return Err(CanonicalLoweringError::UnsupportedExpression("checked select endpoint fields"));
            }
            let channel_type = shape.fields[variant.first_field].clone();
            let channel_shape = scalar_aggregate_shape(&channel_type, self.definitions)
                .ok_or(CanonicalLoweringError::UnsupportedExpression("checked select channel layout"))?;
            let [channel_variant] = &*channel_shape.variants else {
                return Err(CanonicalLoweringError::UnsupportedExpression("checked select channel variants"));
            };
            if channel_variant.arity != 1 || channel_shape.fields[channel_variant.first_field] != Type::Named("Int".into(), Vec::new()) {
                return Err(CanonicalLoweringError::UnsupportedExpression("checked select channel field"));
            }
            let endpoint = self.lower_expr(endpoint)?;
            let channel_value_type = self.field_value_type(&channel_type)?;
            let channel = self.load_source_aggregate_field(endpoint, endpoint_type, 0, 0,
                variant.first_field, channel_value_type)?;
            channels.push(self.load_source_aggregate_field(channel, &channel_type, 0, 0,
                channel_variant.first_field, ValueType::Int)?);
        }
        let names = ["%checked_select_channel0", "%checked_select_channel1"];
        let saved = names.iter().zip(&channels).map(|(name, value)|
            self.bindings.insert((*name).into(), *value)).collect::<Vec<_>>();
        let elements = names.iter().map(|name| Expr::Var((*name).into())).collect::<Vec<_>>();
        let result = self.lower_list_literal(expression, &elements, None);
        for (name, old) in names.into_iter().zip(saved) {
            if let Some(value) = old { self.bindings.insert(name.into(), value); }
            else { self.bindings.remove(name); }
        }
        result.map(Some)
    }
}
