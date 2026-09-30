//! Scalar `vm.par_map` through checked Int slices and sealed worker sites.
use super::*;
use witchy_wir::canonical_safewir::{PAR_MAP_IMPORT_NAMES, HostBufferExtent};

pub(super) fn catalog(
    catalog: CanonicalAbiCatalog,
    signatures: &mut Vec<FunctionSignature>,
    list: witchy_wir::layout::LayoutId,
) -> Result<CanonicalAbiCatalog, CanonicalLoweringError> {
    let buffer = ValueType::Ref(list);
    let mut ids = [FunctionSignatureId::new(0); 2];
    for (index, access) in [MemoryEffect::Read, MemoryEffect::Write].into_iter().enumerate() {
        let id = FunctionSignatureId::new(u32::try_from(signatures.len())
            .map_err(|_| CanonicalLoweringError::IdOverflow)?);
        let parameters = vec![ValueType::Int, buffer];
        signatures.push(FunctionSignature {
            id,
            parameters: parameters.clone(),
            results: vec![ValueType::Size],
            contract: CallableContract {
                parameters: parameters.into_iter().map(|value_type| ParameterContract {
                    value_type, ownership: ParameterOwnership::Value,
                }).collect(),
                results: vec![ResultContract::Value(ValueType::Size)],
                parameter_effects: BTreeMap::from([(1, vec![access])]),
            },
        });
        ids[index] = id;
    }
    catalog.with_par_map_imports(ids, list)
        .map_err(CanonicalLoweringError::InvalidAuthenticatedAbiCatalog)
}

impl FunctionLowerer<'_> {
    fn par_map_call(&mut self, index: usize, destination: ValueId,
        site: ValueId, buffer: HostCallArgument) -> Result<(), CanonicalLoweringError> {
        let import = *self.host_imports.par_map.get(PAR_MAP_IMPORT_NAMES[index])
            .ok_or(CanonicalLoweringError::UnsupportedExpression("missing authenticated parallel-map import"))?;
        self.push_operation(Operation::HostCall {
            destinations: vec![destination], import,
            arguments: vec![HostCallArgument::Value(site), buffer],
        });
        Ok(())
    }

    pub(super) fn lower_par_map(&mut self, expression: &Expr,
        arguments: &[Expr]) -> Result<ValueId, CanonicalLoweringError> {
        let id = *self.host_imports.par_map_sites.get(&(expression as *const Expr as usize))
            .ok_or(CanonicalLoweringError::UnsupportedExpression("parallel-map call has no exact checked source site"))?;
        let [inputs, Expr::Var(callback)] = arguments else {
            return Err(CanonicalLoweringError::UnsupportedExpression("parallel-map arguments"));
        };
        if self.bindings.contains_key(callback) {
            return Err(CanonicalLoweringError::UnsupportedExpression("parallel-map worker callback is shadowed by a local value"));
        }
        let inputs = self.lower_expr(inputs)?;
        self.ensure_list_extent(inputs)?;
        let input_count = *self.list_counts.get(&inputs)
            .ok_or(CanonicalLoweringError::UnsupportedExpression("parallel-map input extent"))?;
        let list_type = self.expression_type(expression)?;
        let (layout, element, header_bytes, stride_bytes) = list_layout(
            &list_type, self.layout_bundle, self.layouts, &self.closures.layouts, self.definitions,
        )?;
        if element != ValueType::Int || stride_bytes != 8 {
            return Err(CanonicalLoweringError::UnsupportedExpression("parallel-map worker requires List(Int)"));
        }
        let zero = self.push_const_size(0)?;
        let site = self.push_const_int(i64::from(id))?;
        let input_slice = HostCallArgument::Buffer {
            place: witchy_wir::canonical_safewir::Place::projected(inputs,
                Projection::Slice { start: zero, length: input_count }, ValueType::Ref(layout), AccessMode::Read),
            extent: HostBufferExtent::Slice,
        };
        let result_count = self.fresh(ValueType::Size)?;
        self.par_map_call(0, result_count, site, input_slice)?;
        self.guard_allocation_capacity(result_count, header_bytes, stride_bytes)?;
        let output = self.fresh(ValueType::Ref(layout))?;
        self.push_operation(Operation::Alloc {
            destination: output, layout,
            extent: witchy_wir::canonical_safewir::Extent::Counted {
                header_bytes, count: zero, capacity: result_count, stride_bytes,
            },
            class: witchy_wir::canonical_safewir::AllocationClass::ReferenceCounted,
        });
        self.set_tracked_lanes(output, zero, result_count);
        let output_slice = HostCallArgument::Buffer {
            place: witchy_wir::canonical_safewir::Place::projected(output,
                Projection::Slice { start: zero, length: result_count }, ValueType::Ref(layout), AccessMode::Write),
            extent: HostBufferExtent::Slice,
        };
        let written = self.fresh(ValueType::Size)?;
        self.par_map_call(1, written, site, output_slice)?;
        let within_capacity = self.push_compare(ComparisonOperator::LessEqualUnsigned, written, result_count)?;
        self.push_guard(within_capacity, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.push_operation(Operation::UpdateCount { object: output, count: written });
        self.set_tracked_lanes(output, written, result_count);
        Ok(output)
    }
}
