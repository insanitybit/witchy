//! Exec strings cross the authenticated host boundary as initialized slices.
use super::*;
use witchy_wir::canonical_safewir::{EXEC_CAPABILITY_KIND, EXEC_RESPONSE_KIND, EXEC_IMPORT_NAMES, DIR_CAPABILITY_KIND};

pub(super) fn is_needed(module: &witchy_syntax::ast::Module) -> bool {
    let scope = witchy_types::pipeline::canonical_scope(module);
    let mut needed = false;
    for item in &module.items {
        let Item::Function(function) = item else { continue };
        if !scope.contains(&function.name) { continue; }
        witchy_syntax::ast::visit::visit_block::<()>(&function.body, &mut |expression| {
            needed |= matches!(expression, Expr::Call { name, .. } if name == "__capop.exec");
            Ok(())
        }).expect("infallible Exec import census");
    }
    needed
}

pub(super) fn catalog(catalog: CanonicalAbiCatalog, signatures: &mut Vec<FunctionSignature>, layout: witchy_wir::layout::LayoutId) -> Result<CanonicalAbiCatalog, CanonicalLoweringError> {
    let text = ValueType::Ref(layout);
    let response = ValueType::Capability(EXEC_RESPONSE_KIND);
    let shapes = [
        (vec![ValueType::Capability(EXEC_CAPABILITY_KIND), ValueType::Capability(DIR_CAPABILITY_KIND), text, text, text], vec![response], false),
        (vec![response], vec![ValueType::Size], false),
        (vec![response, text], vec![ValueType::Size], true),
    ];
    let mut ids = [FunctionSignatureId::new(0); 3];
    for (index, (parameters, results, writes)) in shapes.into_iter().enumerate() {
        let id = FunctionSignatureId::new(u32::try_from(signatures.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?);
        let parameter_effects = parameters.iter().enumerate().filter_map(|(index, ty)| matches!(ty, ValueType::Ref(_))
            .then_some((index as u32, vec![if writes { MemoryEffect::Write } else { MemoryEffect::Read }]))).collect();
        let contract = CallableContract {
            parameters: parameters.iter().map(|ty| ParameterContract { value_type: *ty, ownership: ParameterOwnership::Value }).collect(),
            results: results.iter().map(|ty| ResultContract::Value(*ty)).collect(), parameter_effects,
        };
        signatures.push(FunctionSignature { id, parameters, results, contract });
        ids[index] = id;
    }
    catalog.with_exec_imports(ids, layout).map_err(CanonicalLoweringError::InvalidAuthenticatedAbiCatalog)
}

impl FunctionLowerer<'_> {
    pub(super) fn lower_exec(&mut self, args: &[Expr]) -> Result<ValueId, CanonicalLoweringError> {
        if args.len() != 5 { return Err(CanonicalLoweringError::CallArityMismatch { callee: "__capop.exec".into(), expected: 5, actual: args.len() }); }
        let exec = self.lower_expr(&args[0])?;
        let dir = self.lower_expr(&args[1])?;
        if self.values[&exec] != ValueType::Capability(EXEC_CAPABILITY_KIND) || self.values[&dir] != ValueType::Capability(DIR_CAPABILITY_KIND) {
            return Err(CanonicalLoweringError::UnsupportedExpression("Exec requires authenticated Exec and Dir receivers"));
        }
        let layout = self.string_layout_id()?;
        let mut arguments = vec![HostCallArgument::Capability(exec), HostCallArgument::Capability(dir)];
        for argument in &args[2..] {
            let text = self.lower_string_argument(argument)?;
            arguments.push(self.string_slice_argument(text, layout)?);
        }
        let imports = EXEC_IMPORT_NAMES.map(|name| self.host_imports.exec.get(name).copied()
            .ok_or(CanonicalLoweringError::UnsupportedExpression("missing authenticated Exec import")));
        let [run, size, copy] = imports;
        let response = self.fresh(ValueType::Capability(EXEC_RESPONSE_KIND))?;
        self.push_operation(Operation::HostCall { destinations: vec![response], import: run?, arguments });
        let length = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::HostCall { destinations: vec![length], import: size?, arguments: vec![HostCallArgument::Capability(response)] });
        let capacity = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::MoveValue { destination: capacity, source: length });
        let object = self.alloc_bytes(length, capacity)?;
        let zero = self.push_const_size(0)?;
        let written = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::HostCall { destinations: vec![written], import: copy?, arguments: vec![HostCallArgument::Capability(response), HostCallArgument::Buffer {
            place: witchy_wir::canonical_safewir::Place::projected(object, Projection::Slice { start: zero, length: capacity }, ValueType::Ref(layout), AccessMode::Write),
            extent: HostBufferExtent::Slice,
        }] });
        let fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, written, capacity)?;
        self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.push_operation(Operation::UpdateCount { object, count: written });
        self.string_lengths.insert(object, written);
        self.string_capacities.insert(object, capacity);
        Ok(object)
    }
}
