//! Copy-correct indexed replacement through ordinary typed memory operations.
use super::*;

impl FunctionLowerer<'_> {
    pub(super) fn lower_list_set_at(&mut self, args: &[Expr]) -> Result<ValueId, CanonicalLoweringError> {
        let [source_expr, index_expr, replacement_expr] = args else {
            return Err(CanonicalLoweringError::CallArityMismatch {
                callee: witchy_syntax::intrinsics::LIST_SET_AT.to_owned(), expected: 3, actual: args.len(),
            });
        };
        let list_type = self.expression_type(source_expr)?;
        // Evaluate all source arguments before the bounds failure, in order.
        let source = self.lower_expr(source_expr)?;
        let index = self.lower_expr(index_expr)?;
        let replacement = self.lower_expr(replacement_expr)?;
        self.replace_list_value(source, index, replacement, list_type)
    }

    /// The same checked replacement for already evaluated place coordinates.
    pub(super) fn replace_list_value(
        &mut self,
        source: ValueId,
        index: ValueId,
        replacement: ValueId,
        list_type: Type,
    ) -> Result<ValueId, CanonicalLoweringError> {
        if let Some(family) = self.owner_list_family(source).cloned() {
            if self.values[&index] != ValueType::Int
                || self.values[&replacement] != ValueType::Gc(family.owner_type)
            {
                return Err(CanonicalLoweringError::UnsupportedListElement(list_type));
            }
            let index_size = self.fresh(ValueType::Size)?;
            self.push_operation(Operation::NarrowSize { destination: index_size, value: index });
            let length = self.owner_list_length(source, family.signature)?;
            let in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, index_size, length)?;
            self.push_guard(in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
            let list = self.clone_reference_token(source)?;
            let value = self.clone_reference_token(replacement)?;
            let destination = self.fresh(ValueType::Gc(family.list_type))?;
            self.push_operation(Operation::ClosureOwner(OwnerOperation::ListSet {
                destination,
                list,
                index: index_size,
                value,
                family: family.signature,
            }));
            return Ok(destination);
        }
        let (layout, element_type, header_bytes, stride_bytes) = list_layout(
            &list_type, self.layout_bundle, self.layouts, &self.closures.layouts, self.definitions,
        )?;
        let Type::Named(_, arguments) = list_type.unqualified() else {
            return Err(CanonicalLoweringError::UnsupportedListElement(list_type));
        };
        let [element_source_type] = arguments.as_slice() else {
            return Err(CanonicalLoweringError::UnsupportedListElement(list_type));
        };
        if self.values[&source] != ValueType::Ref(layout)
            || self.values[&index] != ValueType::Int
            || self.values[&replacement] != element_type
        {
            return Err(CanonicalLoweringError::UnsupportedListElement(list_type));
        }
        self.ensure_list_extent(source)?;
        let (length, capacity) = self.tracked_lanes(source).ok_or(
            CanonicalLoweringError::UnsupportedExpression("list.set_at source extent"),
        )?;
        let index_size = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::NarrowSize { destination: index_size, value: index });
        let in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, index_size, length)?;
        self.push_guard(in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.guard_dynamic_extent(length, capacity, header_bytes, stride_bytes)?;
        let zero = self.push_const_size(0)?;
        let object = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::Alloc {
            destination: object, layout,
            extent: witchy_wir::canonical_safewir::Extent::Counted {
                header_bytes, count: zero, capacity, stride_bytes,
            },
            class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted,
        });
        self.set_tracked_lanes(object, zero, capacity);
        let facts = self.snapshot_facts();
        let lanes = [
            (ValueType::Ref(layout), Some(Ownership::Unique)),
            (ValueType::Size, None), (ValueType::Size, None),
        ];
        let header = self.new_block_owned(&lanes)?;
        let body = self.new_block_owned(&lanes)?;
        let exit = self.new_block_owned(&lanes)?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![object, zero, capacity] })?;

        self.current = header;
        let header_values = self.block_parameter_values(header);
        self.set_tracked_lanes(header_values[0], header_values[1], header_values[2]);
        let more = self.push_compare(ComparisonOperator::LessUnsigned, header_values[1], length)?;
        self.finish_current(Terminator::BranchIf {
            condition: more, then_target: body, then_arguments: header_values.clone(),
            else_target: exit, else_arguments: header_values,
        })?;

        self.current = body;
        let body_values = self.block_parameter_values(body);
        let (output, cursor, output_capacity) = (body_values[0], body_values[1], body_values[2]);
        self.set_tracked_lanes(output, cursor, output_capacity);
        let fits = self.push_compare(ComparisonOperator::LessUnsigned, cursor, output_capacity)?;
        self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let maximum = self.push_const_size(u64::from(u32::MAX))?;
        let increment_fits = self.push_compare(ComparisonOperator::LessUnsigned, cursor, maximum)?;
        self.push_guard(increment_fits, witchy_wir::canonical_safewir::GuardKind::Arithmetic);
        let one = self.push_const_size(1)?;
        let next = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor, one, OverflowBehavior::Wrap)?;
        let next_fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, next, output_capacity)?;
        self.push_guard(next_fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let replace = self.push_compare(ComparisonOperator::Equal, cursor, index_size)?;
        let replace_block = self.new_block(&[])?;
        let copy_block = self.new_block(&[])?;
        let selected = self.new_block_owned(&lanes)?;
        let pointer = matches!(element_type, ValueType::Ref(_));
        let selection_facts = self.snapshot_facts();
        self.finish_current(Terminator::BranchIf {
            condition: replace, then_target: replace_block, then_arguments: Vec::new(),
            else_target: copy_block, else_arguments: Vec::new(),
        })?;

        // Store while the arm still holds the child's complete extent facts.
        // A bare child Ref phi loses a nested list's initialized-prefix proof;
        // the completed output list instead crosses the join with all lanes.
        self.current = replace_block;
        let value = if pointer { self.owned_for_storage(replacement, element_source_type)? } else { replacement };
        self.store_replacement_element(output, cursor, value, next, element_type);
        self.finish_current(Terminator::Branch { target: selected, arguments: vec![output, next, output_capacity] })?;

        self.restore_facts(selection_facts.clone(), copy_block);
        self.current = copy_block;
        let value = self.load_list_element(source, cursor, element_type)?;
        let value = if pointer { self.owned_for_storage(value, element_source_type)? } else { value };
        self.store_replacement_element(output, cursor, value, next, element_type);
        self.finish_current(Terminator::Branch { target: selected, arguments: vec![output, next, output_capacity] })?;

        self.restore_facts(selection_facts, selected);
        self.current = selected;
        let selected_values = self.block_parameter_values(selected);
        self.set_tracked_lanes(selected_values[0], selected_values[1], selected_values[2]);
        self.finish_current(Terminator::Branch { target: header, arguments: selected_values })?;

        self.restore_facts(facts, exit);
        self.current = exit;
        let result = self.block_parameter_values(exit);
        self.set_tracked_lanes(result[0], result[1], result[2]);
        Ok(result[0])
    }

    fn store_replacement_element(&mut self, output: ValueId, index: ValueId, value: ValueId, next: ValueId, element_type: ValueType) {
        self.push_operation(Operation::Store {
            place: witchy_wir::canonical_safewir::Place::projected(
                output, Projection::Element { index }, element_type, AccessMode::Write,
            ),
            value,
            transfer: if matches!(element_type, ValueType::Ref(_)) { witchy_wir::canonical_safewir::StoreTransfer::MoveUnique }
                else { witchy_wir::canonical_safewir::StoreTransfer::CopyScalar },
        });
        self.push_operation(Operation::UpdateCount { object: output, count: next });
    }
}
