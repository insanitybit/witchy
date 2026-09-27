//! Copy-correct indexed replacement through ordinary typed memory operations.
use super::*;

impl FunctionLowerer<'_> {
    pub(super) fn lower_list_set_at(&mut self, expression: &Expr, args: &[Expr]) -> Result<ValueId, CanonicalLoweringError> {
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
        let site = self.diagnostic_site(expression)?;
        self.replace_list_value(source, index, replacement, list_type, &site)
    }

    /// The same checked replacement for already evaluated place coordinates.
    pub(super) fn replace_list_value(
        &mut self,
        source: ValueId,
        index: ValueId,
        replacement: ValueId,
        list_type: Type,
        site: &abort::Site,
    ) -> Result<ValueId, CanonicalLoweringError> {
        self.diagnose_list_index(source, index, site)?;
        self.rebuild_list_value(source, index, replacement, list_type)
    }

    /// Internal replacement also used for a dictionary's successful key
    /// lookup. Keep every typed extent and bounds check here; only the
    /// source-level list-index diagnostic belongs to the wrapper above.
    pub(super) fn rebuild_list_value(
        &mut self,
        source: ValueId,
        index: ValueId,
        replacement: ValueId,
        list_type: Type,
    ) -> Result<ValueId, CanonicalLoweringError> {
        if matches!(StoragePlan::classify(&list_type, self.definitions, |_| Some(0)),
            StoragePlan::MoveOnlyList(_)) {
            return Err(CanonicalLoweringError::UnsupportedExpression(
                "indexed replacement cannot copy move-only list elements",
            ));
        }
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
        self.store_replacement_element(output, cursor, output_capacity, value, next, element_type, element_source_type)?;
        self.finish_current(Terminator::Branch { target: selected, arguments: vec![output, next, output_capacity] })?;

        self.restore_facts(selection_facts.clone(), copy_block);
        self.current = copy_block;
        let value = self.load_list_element(source, cursor, element_type)?;
        let value = if pointer { self.owned_for_storage(value, element_source_type)? } else { value };
        self.store_replacement_element(output, cursor, output_capacity, value, next, element_type, element_source_type)?;
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

    /// Copy every element except one checked index into a new initialized
    /// prefix. The source remains live, including all of its owned children.
    pub(super) fn remove_list_value(
        &mut self,
        source: ValueId,
        index: ValueId,
        list_type: Type,
    ) -> Result<ValueId, CanonicalLoweringError> {
        let (layout, element_type, header_bytes, stride_bytes) = list_layout(
            &list_type, self.layout_bundle, self.layouts, &self.closures.layouts, self.definitions,
        )?;
        let Type::Named(_, arguments) = list_type.unqualified() else {
            return Err(CanonicalLoweringError::UnsupportedListElement(list_type));
        };
        let [element_source_type] = arguments.as_slice() else {
            return Err(CanonicalLoweringError::UnsupportedListElement(list_type));
        };
        if self.values[&source] != ValueType::Ref(layout) || self.values[&index] != ValueType::Size {
            return Err(CanonicalLoweringError::UnsupportedListElement(list_type));
        }
        self.ensure_list_extent(source)?;
        let (length, _) = self.tracked_lanes(source).ok_or(
            CanonicalLoweringError::UnsupportedExpression("list removal source extent"),
        )?;
        let in_bounds = self.push_compare(ComparisonOperator::LessUnsigned, index, length)?;
        self.push_guard(in_bounds, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let zero = self.push_const_size(0)?;
        let one = self.push_const_size(1)?;
        let nonempty = self.push_compare(ComparisonOperator::LessUnsigned, zero, length)?;
        self.push_guard(nonempty, witchy_wir::canonical_safewir::GuardKind::Arithmetic);
        let remaining = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Subtract, length, one, OverflowBehavior::Wrap)?;
        self.guard_dynamic_extent(remaining, remaining, header_bytes, stride_bytes)?;
        let object = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::Alloc {
            destination: object, layout,
            extent: witchy_wir::canonical_safewir::Extent::Counted {
                header_bytes, count: zero, capacity: remaining, stride_bytes,
            },
            class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted,
        });
        self.set_tracked_lanes(object, zero, remaining);
        let facts = self.snapshot_facts();
        let lanes = [
            (ValueType::Ref(layout), Some(Ownership::Unique)),
            (ValueType::Size, None), (ValueType::Size, None),
        ];
        let header = self.new_block_owned(&lanes)?;
        let body = self.new_block_owned(&lanes)?;
        let exit = self.new_block_owned(&lanes)?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![object, zero, remaining] })?;
        self.current = header;
        let header_values = self.block_parameter_values(header);
        self.set_tracked_lanes(header_values[0], header_values[1], header_values[2]);
        let more = self.push_compare(ComparisonOperator::LessUnsigned, header_values[1], remaining)?;
        self.finish_current(Terminator::BranchIf {
            condition: more, then_target: body, then_arguments: header_values.clone(),
            else_target: exit, else_arguments: header_values,
        })?;

        self.current = body;
        let body_values = self.block_parameter_values(body);
        let (output, cursor, capacity) = (body_values[0], body_values[1], body_values[2]);
        self.set_tracked_lanes(output, cursor, capacity);
        let fits = self.push_compare(ComparisonOperator::LessUnsigned, cursor, capacity)?;
        self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let maximum = self.push_const_size(u64::from(u32::MAX))?;
        let increment_fits = self.push_compare(ComparisonOperator::LessUnsigned, cursor, maximum)?;
        self.push_guard(increment_fits, witchy_wir::canonical_safewir::GuardKind::Arithmetic);
        let next = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, cursor, one, OverflowBehavior::Wrap)?;
        let next_fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, next, capacity)?;
        self.push_guard(next_fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        let prefix = self.new_block(&[])?;
        let suffix = self.new_block(&[])?;
        let selected = self.new_block_owned(&lanes)?;
        let before_removed = self.push_compare(ComparisonOperator::LessUnsigned, cursor, index)?;
        let selection_facts = self.snapshot_facts();
        self.finish_current(Terminator::BranchIf {
            condition: before_removed, then_target: prefix, then_arguments: Vec::new(),
            else_target: suffix, else_arguments: Vec::new(),
        })?;
        for (block, source_index) in [(prefix, cursor), (suffix, next)] {
            self.restore_facts(selection_facts.clone(), block);
            self.current = block;
            let readable = self.push_compare(ComparisonOperator::LessUnsigned, source_index, length)?;
            self.push_guard(readable, witchy_wir::canonical_safewir::GuardKind::Bounds);
            let value = self.load_list_element(source, source_index, element_type)?;
            let value = if matches!(element_type, ValueType::Ref(_)) {
                self.owned_for_storage(value, element_source_type)?
            } else {
                value
            };
            self.store_replacement_element(output, cursor, capacity, value, next, element_type, element_source_type)?;
            self.finish_current(Terminator::Branch { target: selected, arguments: vec![output, next, capacity] })?;
        }
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

    fn store_replacement_element(
        &mut self,
        output: ValueId,
        index: ValueId,
        capacity: ValueId,
        value: ValueId,
        next: ValueId,
        element_type: ValueType,
        element_source_type: &Type,
    ) -> Result<(), CanonicalLoweringError> {
        // This arm can be revisited after the output loop's backedge has
        // advanced its owner epoch. Establish the write bound in the arm,
        // immediately before the Store, rather than relying on a guard before
        // the selection branch.
        let fits = self.push_compare(ComparisonOperator::LessUnsigned, index, capacity)?;
        self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.push_operation(Operation::Store {
            place: witchy_wir::canonical_safewir::Place::projected(
                output, Projection::Element { index }, element_type, AccessMode::Write,
            ),
            value,
            transfer: if matches!(StoragePlan::classify(element_source_type, self.definitions, |_| Some(0)),
                StoragePlan::RootedView(RootedViewStorage::RetainedRoot(_)))
                && !retained_byte_buffer_view(element_source_type, self.definitions) {
                witchy_wir::canonical_safewir::StoreTransfer::CloneShared
            } else if matches!(element_type, ValueType::Ref(_)) { witchy_wir::canonical_safewir::StoreTransfer::MoveUnique }
                else { witchy_wir::canonical_safewir::StoreTransfer::CopyScalar },
        });
        self.push_operation(Operation::UpdateCount { object: output, count: next });
        Ok(())
    }
}
