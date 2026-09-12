//! Catalogued list extraction: the ordinary Option result and the replacement
//! list are independent lanes. TakeLast transfers one owning edge and publishes
//! the shorter initialized prefix before any allocation or cleanup can run.
use super::*;

impl FunctionLowerer<'_> {
    pub(super) fn lower_list_pop_extract(
        &mut self,
        expression: &Expr,
        arguments: &[Expr],
    ) -> Result<ValueId, CanonicalLoweringError> {
        let [receiver] = arguments else {
            return Err(CanonicalLoweringError::CallArityMismatch {
                callee: witchy_syntax::intrinsics::LIST_POP_EXTRACT.to_owned(),
                expected: 1, actual: arguments.len(),
            });
        };
        let source_type = self.expression_type(receiver)?;
        let result_type = self.expression_type(expression)?;
        let (layout, element_type, _header_bytes, _stride_bytes) = list_layout(
            &source_type, self.layout_bundle, self.layouts, &self.closures.layouts, self.definitions,
        )?;
        let Type::Named(_, elements) = source_type.unqualified() else {
            return Err(CanonicalLoweringError::UnsupportedListElement(source_type));
        };
        let [element_source] = elements.as_slice() else {
            return Err(CanonicalLoweringError::UnsupportedListElement(source_type));
        };
        let option = scalar_aggregate_shape(&result_type, self.definitions)
            .filter(|shape| {
                matches!(result_type.unqualified(), Type::Named(name, args)
                    if name == "Option" && args.len() == 1
                        && args[0].unqualified() == element_source.unqualified())
                    && shape.name == "Option" && shape.tagged
                    && shape.fields.len() == 2 && shape.variants.len() == 2
                    && shape.variants[0].first_field == 1 && shape.variants[0].arity == 1
                    && shape.variants[1].arity == 0
            })
            .ok_or_else(|| CanonicalLoweringError::UnsupportedType(result_type.clone()))?;
        let result_value_type = self.field_value_type(&result_type)?;
        if !matches!(result_value_type, ValueType::Ref(_))
            || self.field_value_type(&option.fields[1])? != element_type
        {
            return Err(CanonicalLoweringError::UnsupportedType(result_type));
        }

        // Coordinates belong to this invocation, including nested calls in an
        // index expression. Rebuilding uses the current root, not a saved parent.
        let place = self.prepare_write_back(receiver)?;
        let source = self.read_prepared_write_back(&place)?;
        if self.values[&source] != ValueType::Ref(layout) {
            return Err(CanonicalLoweringError::UnsupportedListElement(source_type));
        }
        self.ensure_list_extent(source)?;
        let (length, capacity) = self.tracked_lanes(source).ok_or(
            CanonicalLoweringError::UnsupportedExpression("list.pop source extent"),
        )?;
        let zero = self.push_const_size(0)?;
        let nonempty = self.push_compare(ComparisonOperator::LessUnsigned, zero, length)?;
        let facts = self.snapshot_facts();
        let present = self.new_block(&[])?;
        let absent = self.new_block(&[])?;
        let join = self.new_block_owned(&[
            (result_value_type, Some(Ownership::Unique)),
            (ValueType::Ref(layout), Some(Ownership::Unique)),
            (ValueType::Size, None), (ValueType::Size, None),
        ])?;
        self.finish_current(Terminator::BranchIf {
            condition: nonempty,
            then_target: present, then_arguments: Vec::new(),
            else_target: absent, else_arguments: Vec::new(),
        })?;

        self.current = present;
        // Explicit arithmetic evidence also keeps zero-length subtraction off
        // this path. The tail load emits its own index < logical count guard.
        let one = self.push_const_size(1)?;
        let has_element = self.push_compare(ComparisonOperator::LessEqualUnsigned, one, length)?;
        self.push_guard(has_element, witchy_wir::canonical_safewir::GuardKind::Arithmetic);
        let remaining = self.push_arithmetic(
            ValueType::Size, ArithmeticOperator::Subtract, length, one, OverflowBehavior::Wrap,
        )?;
        let in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, remaining, length)?;
        self.push_guard(in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let payload = self.fresh(element_type)?;
        self.push_operation(Operation::Load {
            destination: payload,
            place: witchy_wir::canonical_safewir::Place::projected(
                source, Projection::Element { index: remaining }, element_type, AccessMode::Read,
            ),
            transfer: witchy_wir::canonical_safewir::LoadTransfer::TakeLast,
        });
        self.set_tracked_lanes(source, remaining, capacity);
        // The typed aggregate constructor retains a reference payload and
        // initializes inactive nullable slots; scalar payloads are copied.
        let some = self.lower_aggregate_for_source(&result_type, &option, 0, vec![payload])?;
        self.finish_current(Terminator::Branch {
            target: join, arguments: vec![some, source, remaining, capacity],
        })?;

        self.restore_facts(facts.clone(), absent);
        self.current = absent;
        let none = self.lower_aggregate_for_source(&result_type, &option, 1, Vec::new())?;
        // Empty pop does not copy the list or inspect an inactive payload.
        // Move the original token through the join, without retaining it.
        let unchanged = source;
        self.finish_current(Terminator::Branch {
            target: join, arguments: vec![none, unchanged, length, capacity],
        })?;

        self.restore_facts(facts, join);
        self.current = join;
        let values = self.block_parameter_values(join);
        self.token_join_parameters.insert(values[0]);
        self.token_join_parameters.insert(values[1]);
        self.set_tracked_lanes(values[1], values[2], values[3]);
        self.commit_prepared_write_back(&place, values[1])?;
        Ok(values[0])
    }

}
