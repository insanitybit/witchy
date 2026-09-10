//! Checked Net operations over authenticated references and ordinary slices.
use super::*;
use witchy_wir::canonical_safewir::{NET_CAPABILITY_KIND, LISTENER_CAPABILITY_KIND, SOCKET_CAPABILITY_KIND, NET_IMPORT_NAMES};

fn operation(name: &str) -> Option<(usize, witchy_wir::canonical_safewir::CapabilityKindId, usize)> {
    Some(match name {
        "__capop.listen" => (0, NET_CAPABILITY_KIND, 2),
        "__capop.accept" => (1, LISTENER_CAPABILITY_KIND, 1),
        "__capop.send_bytes" => (2, SOCKET_CAPABILITY_KIND, 2),
        "__capop.recv_line" => (3, SOCKET_CAPABILITY_KIND, 1),
        "__capop.recv_bytes" => (4, SOCKET_CAPABILITY_KIND, 2),
        "__capop.close" => (6, SOCKET_CAPABILITY_KIND, 1),
        "__capop.serve_pool" => (7, LISTENER_CAPABILITY_KIND, 1),
        _ => return None,
    })
}

pub(super) fn is_operation(name: &str) -> bool { operation(name).is_some() }

pub(super) fn collect_imports(module: &witchy_syntax::ast::Module) -> BTreeSet<&'static str> {
    let scope = witchy_types::pipeline::canonical_scope(module);
    let mut needed = BTreeSet::new();
    for item in &module.items {
        let Item::Function(function) = item else { continue };
        if !scope.contains(&function.name) { continue; }
        witchy_syntax::ast::visit::visit_block::<()>(&function.body, &mut |expression| {
            if let Expr::Call { name, .. } = expression && let Some((index, _, _)) = operation(name) {
                needed.insert(NET_IMPORT_NAMES[index]);
                if index == 3 || index == 4 { needed.insert(NET_IMPORT_NAMES[5]); }
            }
            Ok(())
        }).expect("Net import visitor cannot fail");
    }
    needed
}

pub(super) fn catalog(catalog: CanonicalAbiCatalog, signatures: &mut Vec<FunctionSignature>, layout: witchy_wir::layout::LayoutId) -> Result<CanonicalAbiCatalog, CanonicalLoweringError> {
    let net = ValueType::Capability(NET_CAPABILITY_KIND);
    let listener = ValueType::Capability(LISTENER_CAPABILITY_KIND);
    let socket = ValueType::Capability(SOCKET_CAPABILITY_KIND);
    let text = ValueType::Ref(layout);
    let shapes = [
        (vec![net, text], vec![listener], false),
        (vec![listener], vec![socket], false),
        (vec![socket, text], vec![], false),
        (vec![socket], vec![ValueType::Size], false),
        (vec![socket, ValueType::Int], vec![ValueType::Size], false),
        (vec![socket, text], vec![ValueType::Size], true),
        (vec![socket], vec![], false),
        (vec![listener], vec![], false),
    ];
    let mut ids = [FunctionSignatureId::new(0); 8];
    for (index, (parameters, results, writes)) in shapes.into_iter().enumerate() {
        let id = FunctionSignatureId::new(u32::try_from(signatures.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?);
        let parameter_effects = parameters.iter().enumerate().filter_map(|(index, ty)| {
            matches!(ty, ValueType::Ref(_)).then_some((index as u32, vec![if writes { MemoryEffect::Write } else { MemoryEffect::Read }]))
        }).collect();
        let contract = CallableContract {
            parameters: parameters.iter().map(|ty| ParameterContract { value_type: *ty, ownership: ParameterOwnership::Value }).collect(),
            results: results.iter().map(|ty| ResultContract::Value(*ty)).collect(), parameter_effects,
        };
        signatures.push(FunctionSignature { id, parameters, results, contract });
        ids[index] = id;
    }
    catalog.with_net_imports(ids, layout).map_err(CanonicalLoweringError::InvalidAuthenticatedAbiCatalog)
}

impl FunctionLowerer<'_> {
    pub(super) fn lower_net(&mut self, name: &str, args: &[Expr]) -> Result<Option<ValueId>, CanonicalLoweringError> {
        let (index, kind, arity) = operation(name).ok_or(CanonicalLoweringError::UnsupportedExpression("Net operation"))?;
        if args.len() != arity {
            return Err(CanonicalLoweringError::CallArityMismatch { callee: name.to_owned(), expected: arity, actual: args.len() });
        }
        let token = self.lower_expr(&args[0])?;
        if self.values[&token] != ValueType::Capability(kind) {
            return Err(CanonicalLoweringError::UnsupportedExpression("Net operation requires its authenticated receiver kind"));
        }
        let import = *self.host_imports.net.get(NET_IMPORT_NAMES[index])
            .ok_or(CanonicalLoweringError::UnsupportedExpression("missing authenticated Net import"))?;
        let mut arguments = vec![HostCallArgument::Capability(token)];
        if index == 0 || index == 2 {
            let text = self.lower_string_argument(&args[1])?;
            let layout = self.string_layout_id()?;
            arguments.push(self.string_slice_argument(text, layout)?);
        } else if index == 4 {
            let count = self.lower_expr(&args[1])?;
            if self.values[&count] != ValueType::Int {
                return Err(CanonicalLoweringError::UnsupportedExpression("Net receive byte count"));
            }
            arguments.push(HostCallArgument::Value(count));
        }
        let result_type = match index {
            0 => Some(ValueType::Capability(LISTENER_CAPABILITY_KIND)),
            1 => Some(ValueType::Capability(SOCKET_CAPABILITY_KIND)),
            3 | 4 => Some(ValueType::Size),
            _ => None,
        };
        let result = result_type.map(|ty| self.fresh(ty)).transpose()?;
        self.push_operation(Operation::HostCall { destinations: result.into_iter().collect(), import, arguments });
        if index != 3 && index != 4 { return Ok(result); }

        // The size call reads ONCE and stages bytes on this authenticated
        // socket. The fill cannot consume another socket's pending receive.
        let length = result.expect("receive size result");
        let capacity = self.fresh(ValueType::Size)?;
        self.push_operation(Operation::MoveValue { destination: capacity, source: length });
        let object = self.alloc_bytes(length, capacity)?;
        let zero = self.push_const_size(0)?;
        let written = self.fresh(ValueType::Size)?;
        let layout = self.string_layout_id()?;
        let import = *self.host_imports.net.get(NET_IMPORT_NAMES[5])
            .ok_or(CanonicalLoweringError::UnsupportedExpression("missing authenticated Net receive fill"))?;
        self.push_operation(Operation::HostCall {
            destinations: vec![written], import,
            arguments: vec![HostCallArgument::Capability(token), HostCallArgument::Buffer {
                place: witchy_wir::canonical_safewir::Place::projected(object, Projection::Slice { start: zero, length: capacity }, ValueType::Ref(layout), AccessMode::Write),
                extent: HostBufferExtent::Slice,
            }],
        });
        let fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, written, capacity)?;
        self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.push_operation(Operation::UpdateCount { object, count: written });
        self.string_lengths.insert(object, written);
        self.string_capacities.insert(object, capacity);
        Ok(Some(object))
    }
}
