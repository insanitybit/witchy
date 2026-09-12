//! Catalogued list extraction: the ordinary Option result and the replacement
//! list are independent lanes. Copying the prefix preserves every source alias;
//! no input header is shortened while it still owns the removed element.
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
        let (layout, element_type, header_bytes, stride_bytes) = list_layout(
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
            (ValueType::Ref(layout), Some(Ownership::Shared)),
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
        let repaired = self.copy_pop_prefix(
            source, remaining, layout, element_type, element_source, header_bytes, stride_bytes,
        )?;
        let payload = self.load_list_element(source, remaining, element_type)?;
        // The typed aggregate constructor retains a reference payload and
        // initializes inactive nullable slots; scalar payloads are copied.
        let some = self.lower_aggregate_for_source(&result_type, &option, 0, vec![payload])?;
        let (new_length, new_capacity) = self.tracked_lanes(repaired).ok_or(
            CanonicalLoweringError::UnsupportedExpression("list.pop repaired extent"),
        )?;
        self.finish_current(Terminator::Branch {
            target: join, arguments: vec![some, repaired, new_length, new_capacity],
        })?;

        self.restore_facts(facts.clone(), absent);
        self.current = absent;
        let none = self.lower_aggregate_for_source(&result_type, &option, 1, Vec::new())?;
        // Empty pop does not copy the list or inspect an inactive payload.
        // A separate token permits the same join even when the root is shared.
        let unchanged = self.clone_reference_token(source)?;
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

    /// Existing range operations require exact initialized-range evidence for
    /// pointer elements. A shortened dynamic prefix instead uses the ordinary
    /// bounded load/store loop, publishing each initialized slot in order.
    #[allow(clippy::too_many_arguments)]
    fn copy_pop_prefix(
        &mut self,
        source: ValueId,
        count: ValueId,
        layout: witchy_wir::layout::LayoutId,
        element_type: ValueType,
        element_source: &Type,
        header_bytes: u32,
        stride_bytes: u32,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let zero = self.push_const_size(0)?;
        self.guard_dynamic_extent(zero, count, header_bytes, stride_bytes)?;
        let object = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::Alloc {
            destination: object, layout,
            extent: witchy_wir::canonical_safewir::Extent::Counted {
                header_bytes, count: zero, capacity: count, stride_bytes,
            },
            class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted,
        });
        self.set_tracked_lanes(object, zero, count);
        let facts = self.snapshot_facts();
        let lanes = [
            (ValueType::Ref(layout), Some(Ownership::Unique)),
            (ValueType::Size, None), (ValueType::Size, None),
        ];
        let header = self.new_block_owned(&lanes)?;
        let body = self.new_block_owned(&lanes)?;
        let exit = self.new_block_owned(&lanes)?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![object, zero, count] })?;

        self.current = header;
        let values = self.block_parameter_values(header);
        self.set_tracked_lanes(values[0], values[1], values[2]);
        let more = self.push_compare(ComparisonOperator::LessUnsigned, values[1], values[2])?;
        self.finish_current(Terminator::BranchIf {
            condition: more,
            then_target: body, then_arguments: values.clone(),
            else_target: exit, else_arguments: values,
        })?;

        self.current = body;
        let values = self.block_parameter_values(body);
        let (output, cursor, capacity) = (values[0], values[1], values[2]);
        self.set_tracked_lanes(output, cursor, capacity);
        let value = self.load_list_element(source, cursor, element_type)?;
        let pointer = matches!(element_type, ValueType::Ref(_));
        let value = if pointer { self.owned_for_storage(value, element_source)? } else { value };
        let fits = self.push_compare(ComparisonOperator::LessUnsigned, cursor, capacity)?;
        self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let maximum = self.push_const_size(u64::from(u32::MAX))?;
        let increment_fits = self.push_compare(ComparisonOperator::LessUnsigned, cursor, maximum)?;
        self.push_guard(increment_fits, witchy_wir::canonical_safewir::GuardKind::Arithmetic);
        let one = self.push_const_size(1)?;
        let next = self.push_arithmetic(
            ValueType::Size, ArithmeticOperator::Add, cursor, one, OverflowBehavior::Wrap,
        )?;
        let next_fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, next, capacity)?;
        self.push_guard(next_fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        // Finish fallible preparation before installing an owned edge. Publish
        // that edge immediately, before cleanup or the next element can branch.
        self.push_operation(Operation::Store {
            place: witchy_wir::canonical_safewir::Place::projected(
                output, Projection::Element { index: cursor }, element_type, AccessMode::Write,
            ),
            value,
            transfer: if pointer { witchy_wir::canonical_safewir::StoreTransfer::MoveUnique }
                else { witchy_wir::canonical_safewir::StoreTransfer::CopyScalar },
        });
        self.push_operation(Operation::UpdateCount { object: output, count: next });
        self.finish_current(Terminator::Branch { target: header, arguments: vec![output, next, capacity] })?;

        self.restore_facts(facts, exit);
        self.current = exit;
        let result = self.block_parameter_values(exit);
        self.set_tracked_lanes(result[0], result[1], result[2]);
        Ok(result[0])
    }
}
