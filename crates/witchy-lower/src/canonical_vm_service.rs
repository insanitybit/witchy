//! Parent-side lock-step loop over an authenticated isolated service.
use super::*;
use witchy_wir::canonical_safewir::{VM_SERVICE_KIND, VM_SERVICE_RESPONSE_KIND, VM_SERVICE_IMPORT_NAMES};

pub(super) fn catalog(catalog: CanonicalAbiCatalog, signatures: &mut Vec<FunctionSignature>, bytes: witchy_wir::layout::LayoutId) -> Result<CanonicalAbiCatalog, CanonicalLoweringError> {
    let service = ValueType::Capability(VM_SERVICE_KIND);
    let response = ValueType::Capability(VM_SERVICE_RESPONSE_KIND);
    let buffer = ValueType::Ref(bytes);
    let shapes = [(vec![ValueType::Int, buffer], vec![service]), (vec![service, buffer], vec![response]),
        (vec![response], vec![ValueType::Size]), (vec![response, buffer], vec![ValueType::Size]), (vec![service], vec![])];
    let mut ids = [FunctionSignatureId::new(0); 5];
    for (index, (parameters, results)) in shapes.into_iter().enumerate() {
        let id = FunctionSignatureId::new(u32::try_from(signatures.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?);
        let parameter_effects = parameters.iter().enumerate().filter_map(|(slot, ty)| matches!(ty, ValueType::Ref(_))
            .then_some((slot as u32, vec![if index == 3 { MemoryEffect::Write } else { MemoryEffect::Read }]))).collect();
        let contract = CallableContract { parameters: parameters.iter().map(|ty| ParameterContract { value_type: *ty, ownership: ParameterOwnership::Value }).collect(),
            results: results.iter().map(|ty| ResultContract::Value(*ty)).collect(), parameter_effects };
        signatures.push(FunctionSignature { id, parameters, results, contract });
        ids[index] = id;
    }
    catalog.with_vm_service_imports(ids, bytes).map_err(CanonicalLoweringError::InvalidAuthenticatedAbiCatalog)
}
impl FunctionLowerer<'_> {
    fn service_call(&mut self, index: usize, destinations: Vec<ValueId>, arguments: Vec<HostCallArgument>) -> Result<(), CanonicalLoweringError> {
        let import = *self.host_imports.vm_service.get(VM_SERVICE_IMPORT_NAMES[index])
            .ok_or(CanonicalLoweringError::UnsupportedExpression("missing authenticated VM service import"))?;
        self.push_operation(Operation::HostCall { destinations, import, arguments });
        Ok(())
    }
    pub(super) fn lower_vm_service(&mut self, expression: &Expr, args: &[Expr]) -> Result<ValueId, CanonicalLoweringError> {
        let id = *self.host_imports.vm_sites.get(&(expression as *const Expr as usize))
            .ok_or(CanonicalLoweringError::UnsupportedExpression("VM service call has no exact checked source site"))?;
        let [init, requests, Expr::Var(_)] = args else { return Err(CanonicalLoweringError::UnsupportedExpression("VM service arguments")) };
        // Preserve source argument evaluation order before creating the worker.
        let init = self.lower_expr(init)?;
        let requests = self.lower_expr(requests)?;
        self.ensure_string_extent(init)?;
        self.ensure_list_extent(requests)?;
        let count = self.list_counts[&requests];
        let byte_layout = self.string_layout_id()?;
        let site = self.push_const_int(i64::from(id))?;
        let service = self.fresh(ValueType::Capability(VM_SERVICE_KIND))?;
        let init_buffer = self.string_slice_argument(init, byte_layout)?;
        self.service_call(0, vec![service], vec![HostCallArgument::Value(site), init_buffer])?;
        let list_type = self.expression_type(expression)?;
        let (layout, element_type, header_bytes, stride_bytes) = list_layout(&list_type, self.layout_bundle, self.layouts, &self.closures.layouts, self.definitions)?;
        let zero = self.push_const_size(0)?;
        let capacity = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::MoveValue { destination: capacity, source: count });
        self.guard_allocation_capacity(capacity, header_bytes, stride_bytes)?;
        let initial = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::Alloc { destination: initial, layout,
            extent: witchy_wir::canonical_safewir::Extent::Counted { header_bytes, count: zero, capacity, stride_bytes },
            class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted });
        self.set_tracked_lanes(initial, zero, capacity);
        let facts = self.snapshot_facts();
        let header = self.new_block_owned(&[(ValueType::Ref(layout), Some(Ownership::Unique)), (ValueType::Size, None), (ValueType::Size, None), (ValueType::Size, None)])?;
        let body = self.new_block(&[ValueType::Ref(layout), ValueType::Size, ValueType::Size, ValueType::Size])?;
        let exit = self.new_block_owned(&[(ValueType::Ref(layout), Some(Ownership::Unique)), (ValueType::Size, None), (ValueType::Size, None)])?;
        let cursor = self.push_const_size(0)?;
        self.finish_current(Terminator::Branch { target: header, arguments: vec![initial, zero, capacity, cursor] })?;
        self.current = header;
        let state = self.block_parameter_values(header);
        self.set_tracked_lanes(state[0], state[1], state[2]);
        let more = self.push_compare(ComparisonOperator::LessUnsigned, state[3], count)?;
        self.finish_current(Terminator::BranchIf { condition: more, then_target: body, then_arguments: state.clone(), else_target: exit, else_arguments: state[..3].to_vec() })?;
        self.current = body;
        let state = self.block_parameter_values(body);
        self.set_tracked_lanes(state[0], state[1], state[2]);
        let request = self.load_list_element(requests, state[3], element_type)?;
        self.ensure_string_extent(request)?;
        let input = self.string_slice_argument(request, byte_layout)?;
        let response = self.fresh(ValueType::Capability(VM_SERVICE_RESPONSE_KIND))?;
        self.service_call(1, vec![response], vec![HostCallArgument::Capability(service), input])?;
        let length = self.fresh(ValueType::Size)?;
        self.service_call(2, vec![length], vec![HostCallArgument::Capability(response)])?;
        let capacity = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::MoveValue { destination: capacity, source: length });
        let output = self.alloc_bytes(length, capacity)?;
        let start = self.push_const_size(0)?;
        let destination = HostCallArgument::Buffer { place: witchy_wir::canonical_safewir::Place::projected(output, Projection::Slice { start, length: capacity }, ValueType::Ref(byte_layout), AccessMode::Write), extent: HostBufferExtent::Slice };
        let required = self.fresh(ValueType::Size)?;
        self.service_call(3, vec![required], vec![HostCallArgument::Capability(response), destination])?;
        let fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, required, capacity)?;
        self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.push_operation(Operation::UpdateCount { object: output, count: required });
        self.string_lengths.insert(output, required);
        self.string_capacities.insert(output, capacity);
        let next = self.push_owned_element(state[0], output, &list_type)?;
        let (next_count, next_capacity) = self.tracked_lanes(next).ok_or(CanonicalLoweringError::UnsupportedExpression("VM service output list extent"))?;
        let one = self.push_const_size(1)?;
        let next_cursor = self.push_arithmetic(ValueType::Size, ArithmeticOperator::Add, state[3], one, OverflowBehavior::Trap)?;
        let (next_count, next_capacity) = self.split_lanes(next_count, next_capacity)?;
        let back = vec![next, next_count, next_capacity, next_cursor];
        self.demote_unique_lanes_carrying_shared(header, exit, &back);
        self.finish_current(Terminator::Branch { target: header, arguments: back })?;
        self.restore_facts(facts, exit);
        self.current = exit;
        let result = self.block_parameter_values(exit);
        self.set_tracked_lanes(result[0], result[1], result[2]);
        self.service_call(4, vec![], vec![HostCallArgument::Capability(service)])?;
        Ok(result[0])
    }
}
