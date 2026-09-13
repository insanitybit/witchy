//! One authenticated read creates an immutable response. Size/copy do not read
//! again, and only a guarded initialized prefix becomes the returned String.
use super::*;

impl FunctionLowerer<'_> {
    pub(super) fn lower_build_read(&mut self, token: ValueId, path: &Expr) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let path = self.lower_string_argument(path)?;
        let path = self.string_slice_argument(path, layout)?;
        let read = self.capability_import("build_read_at")?;
        let response = self.fresh(ValueType::Capability(witchy_wir::canonical_safewir::BUILD_READ_RESPONSE_KIND))?;
        self.push_operation(Operation::HostCall {
            destinations: vec![response], import: read,
            arguments: vec![HostCallArgument::Capability(token), path],
        });
        self.lower_build_response_string(response)
    }

    pub(super) fn lower_build_env(&mut self, token: ValueId, key: &Expr) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let key = self.lower_string_argument(key)?;
        let key = self.string_slice_argument(key, layout)?;
        let read = self.capability_import("build_env_at")?;
        let test = self.capability_import("build_response_is_some")?;
        let response = self.fresh(ValueType::Capability(witchy_wir::canonical_safewir::BUILD_READ_RESPONSE_KIND))?;
        self.push_operation(Operation::HostCall {
            destinations: vec![response], import: read,
            arguments: vec![HostCallArgument::Capability(token), key],
        });
        let present = self.fresh(ValueType::Bool)?;
        self.push_operation(Operation::HostCall {
            destinations: vec![present], import: test,
            arguments: vec![HostCallArgument::Capability(response)],
        });
        let option_type = Type::Named("Option".into(), vec![Type::Named("String".into(), Vec::new())]);
        let shape = scalar_aggregate_shape(&option_type, self.definitions)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("Option(String) shape"))?;
        let (some, _) = shape.variant("Some").ok_or(CanonicalLoweringError::MissingBinding("Some".into()))?;
        let (none, _) = shape.variant("None").ok_or(CanonicalLoweringError::MissingBinding("None".into()))?;
        let value_type = self.field_value_type(&option_type)?;
        let facts = self.snapshot_facts();
        let some_block = self.new_block(&[])?;
        let none_block = self.new_block(&[])?;
        let join = self.new_block_owned(&[(value_type, Some(Ownership::Unique))])?;
        self.finish_current(Terminator::BranchIf {
            condition: present, then_target: some_block, then_arguments: Vec::new(),
            else_target: none_block, else_arguments: Vec::new(),
        })?;
        self.current = some_block;
        // A None response may not be measured or copied. Build the active
        // variant independently, using the checked aggregate's slot rules.
        let text = self.lower_build_response_string(response)?;
        let value = self.lower_aggregate_for_source(&option_type, &shape, some, vec![text])?;
        self.finish_current(Terminator::Branch { target: join, arguments: vec![value] })?;
        self.restore_facts(facts.clone(), none_block);
        self.current = none_block;
        let value = self.lower_aggregate_for_source(&option_type, &shape, none, Vec::new())?;
        self.finish_current(Terminator::Branch { target: join, arguments: vec![value] })?;
        self.restore_facts(facts, join);
        self.current = join;
        let value = self.block_parameter_values(join)[0];
        self.token_join_parameters.insert(value);
        Ok(value)
    }

    fn lower_build_response_string(&mut self, response: ValueId) -> Result<ValueId, CanonicalLoweringError> {
        let layout = self.string_layout_id()?;
        let size = self.capability_import("build_read_response_size")?;
        let copy = self.capability_import("build_read_response_into")?;
        let length = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::HostCall {
            destinations: vec![length], import: size,
            arguments: vec![HostCallArgument::Capability(response)],
        });
        let capacity = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::MoveValue { destination: capacity, source: length });
        let object = self.alloc_bytes(length, capacity)?;
        let zero = self.push_const_size(0)?;
        let written = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::HostCall {
            destinations: vec![written], import: copy,
            arguments: vec![HostCallArgument::Capability(response), HostCallArgument::Buffer {
                place: witchy_wir::canonical_safewir::Place::projected(object,
                    Projection::Slice { start: zero, length: capacity }, ValueType::Ref(layout), AccessMode::Write),
                extent: HostBufferExtent::Slice,
            }],
        });
        let fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, written, capacity)?;
        self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.push_operation(Operation::UpdateCount { object, count: written });
        self.string_lengths.insert(object, written);
        self.string_capacities.insert(object, capacity);
        Ok(object)
    }
}
