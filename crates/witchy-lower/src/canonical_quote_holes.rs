//! Parser-issued static quote holes become bounded typed handle transport.
//! No aggregate storage is reinterpreted as a host buffer and no source text
//! is passed to the compiler session for reparsing.
use super::*;

impl FunctionLowerer<'_> {
    fn quote_wire_fixed(&mut self, bytes: &[u8]) -> Result<ValueId, CanonicalLoweringError> {
        let zero = self.push_const_size(0)?;
        let capacity = self.push_const_size(bytes.len() as u64)?;
        let object = self.alloc_bytes(zero, capacity)?;
        let one = self.push_const_size(1)?;
        let mut cursor = zero;
        for byte in bytes {
            let value = self.push_const_size(u64::from(*byte))?;
            cursor = self.push_byte_store_and_publish(object, cursor, value, one)?;
        }
        Ok(object)
    }

    fn quote_wire_dynamic_field(
        &mut self,
        wire: ValueId,
        tag: u8,
        handle: ValueId,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let length = self.string_length_of(handle)?;
        let mut header = self.quote_wire_fixed(&[tag, 0, 0, 0, 0])?;
        let mask = self.push_const_size(255)?;
        for index in 0..4 {
            let shift = self.push_const_size(index * 8)?;
            let shifted = self.push_arithmetic(
                ValueType::Size,
                ArithmeticOperator::ShiftRightUnsigned,
                length,
                shift,
                OverflowBehavior::Wrap,
            )?;
            let byte = self.push_arithmetic(
                ValueType::Size,
                ArithmeticOperator::BitAnd,
                shifted,
                mask,
                OverflowBehavior::Wrap,
            )?;
            let byte = self.proven_byte(byte)?;
            let offset = self.push_const_size(index + 1)?;
            self.push_byte_store(header, offset, byte)?;
        }
        let joined = self.concat_string_values(wire, header)?;
        header = joined;
        self.concat_string_values(header, handle)
    }

    pub(super) fn lower_quote_holes(
        &mut self,
        expression: &Expr,
        name: &str,
        args: &[Expr],
        import: witchy_wir::canonical_safewir::ImportId,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let [
            Expr::Str(seed),
            Expr::List(parts),
            Expr::List(holes),
            Expr::Int(site),
        ] = args
        else {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "quote holes require a parser-issued static plan",
            ));
        };
        if *site <= 0
            || holes.is_empty()
            || holes.len() > 256
            || parts.len() != holes.len() + 1
            || !parts.iter().all(|part| matches!(part, Expr::Str(_)))
        {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "quote hole plan shape",
            ));
        }
        let category = if name == witchy_syntax::intrinsics::COMPILER_QUOTE_EXPR_HOLES {
            b'E'
        } else {
            b'I'
        };
        let mut prefix = b"QH1".to_vec();
        prefix.push(category);
        prefix.extend_from_slice(
            &u32::try_from(*site)
                .map_err(|_| CanonicalLoweringError::IdOverflow)?
                .to_le_bytes(),
        );
        prefix.extend_from_slice(
            &u32::try_from(seed.len())
                .map_err(|_| CanonicalLoweringError::IdOverflow)?
                .to_le_bytes(),
        );
        prefix.extend_from_slice(seed.as_bytes());
        prefix.extend_from_slice(
            &u16::try_from(holes.len())
                .map_err(|_| CanonicalLoweringError::IdOverflow)?
                .to_le_bytes(),
        );
        if prefix.len() > 65_536 {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "quote hole plan input limit",
            ));
        }
        let mut wire = self.quote_wire_fixed(&prefix)?;
        let layout = self.string_layout_id()?;
        for hole in holes {
            let Expr::Call { name, args } = hole else {
                return Err(CanonicalLoweringError::UnsupportedExpression(
                    "quote holes require static typed constructors",
                ));
            };
            let [child] = args.as_slice() else {
                return Err(CanonicalLoweringError::UnsupportedExpression(
                    "quote hole constructor arity",
                ));
            };
            let (tag, expected) = match name.as_str() {
                "meta.expr_hole" => (b'E', "meta.CompilerExprSyntax"),
                "meta.type_hole" => (b'T', "meta.CompilerTypeSyntax"),
                "meta.pattern_hole" => (b'P', "meta.CompilerPatternSyntax"),
                _ => {
                    return Err(CanonicalLoweringError::UnsupportedExpression(
                        "quote hole constructor category",
                    ));
                }
            };
            let source = self.expression_storage_type(child)?;
            let shape = scalar_aggregate_shape(&source, self.definitions).ok_or(
                CanonicalLoweringError::UnsupportedExpression("quote hole syntax carrier"),
            )?;
            let (index, variant) =
                shape
                    .variant(expected)
                    .ok_or(CanonicalLoweringError::UnsupportedExpression(
                        "quote hole syntax variant",
                    ))?;
            if shape.variants.len() != 1
                || variant.arity != 1
                || !Self::is_string_type(&shape.fields[variant.first_field])
            {
                return Err(CanonicalLoweringError::UnsupportedExpression(
                    "quote hole syntax payload",
                ));
            }
            let value = self.lower_expr(child)?;
            let handle = self.load_source_aggregate_field(
                value,
                &source,
                index,
                0,
                variant.first_field,
                ValueType::Ref(layout),
            )?;
            self.ensure_string_extent(handle)?;
            wire = self.quote_wire_dynamic_field(wire, tag, handle)?;
        }
        let input = self.string_slice_argument(wire, layout)?;
        let handle = self.lower_bounded_host_result(import, vec![input])?;
        let source = self.expression_storage_type(expression)?;
        let shape = scalar_aggregate_shape(&source, self.definitions).ok_or(
            CanonicalLoweringError::UnsupportedExpression("quote result syntax carrier"),
        )?;
        let expected = if category == b'E' {
            "meta.CompilerExprSyntax"
        } else {
            "meta.ItemSyntax"
        };
        let (index, variant) =
            shape
                .variant(expected)
                .ok_or(CanonicalLoweringError::UnsupportedExpression(
                    "quote result syntax variant",
                ))?;
        if shape.variants.len() != 1
            || variant.arity != 1
            || !Self::is_string_type(&shape.fields[variant.first_field])
        {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "quote result syntax payload",
            ));
        }
        self.lower_aggregate_for_source(&source, &shape, index, vec![handle])
    }
}
