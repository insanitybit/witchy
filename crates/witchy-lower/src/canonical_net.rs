//! Checked Net operations over authenticated references and ordinary slices.
//!
//! Every fallible operation is one host call answering a Net response, read
//! back as `Result(T, CapError)` (RFC-0126 rule 3): the response mints the
//! `Socket` or `Listener`, or carries the received text or resolved addresses.
use super::*;
use witchy_wir::canonical_safewir::{NET_CAPABILITY_KIND, LISTENER_CAPABILITY_KIND, SOCKET_CAPABILITY_KIND, NET_IMPORT_NAMES, CAP_RESPONSE_KIND};

fn operation(name: &str) -> Option<(usize, witchy_wir::canonical_safewir::CapabilityKindId, usize)> {
    Some(match name {
        "__capop.listen" => (0, NET_CAPABILITY_KIND, 2),
        "__capop.accept" => (1, LISTENER_CAPABILITY_KIND, 1),
        "__capop.send_bytes" => (2, SOCKET_CAPABILITY_KIND, 2),
        "__capop.recv_line" => (3, SOCKET_CAPABILITY_KIND, 1),
        "__capop.recv_bytes" => (4, SOCKET_CAPABILITY_KIND, 2),
        "__capop.send_line" => (5, SOCKET_CAPABILITY_KIND, 2),
        "__capop.close" => (6, SOCKET_CAPABILITY_KIND, 1),
        "__capop.serve_pool" => (7, LISTENER_CAPABILITY_KIND, 1),
        "__capop.recv_all" => (8, SOCKET_CAPABILITY_KIND, 1),
        "__capop.connect" => (9, NET_CAPABILITY_KIND, 2),
        "__capop.resolve" => (10, NET_CAPABILITY_KIND, 2),
        _ => return None,
    })
}

/// `close` and `serve_pool` cannot fail; every other operation answers a
/// response.
fn answers_response(index: usize) -> bool { !matches!(index, 6 | 7) }

/// The response getters a fallible Net operation reads.
pub(super) const RESPONSE_IMPORTS: [&str; 5] = [
    "cap_response_status", "cap_response_size", "cap_response_into",
    "cap_response_take_socket", "cap_response_take_listener",
];

pub(super) fn is_operation(name: &str) -> bool { operation(name).is_some() || name == LISTEN_TLS }

/// `net.listen_tls` binds through its own import: it also takes the `Secret`
/// key, so it needs both the Listen right and the Secret.
const LISTEN_TLS: &str = "__capop.listen_tls";

/// Whether the module binds a TLS listener.
pub(super) fn uses_listen_tls(module: &witchy_syntax::ast::Module) -> bool {
    let scope = witchy_canonical_source::scope::canonical_scope(module);
    let mut found = false;
    for item in &module.items {
        let Item::Function(function) = item else { continue };
        if !scope.contains(&function.name) { continue; }
        witchy_syntax::ast::visit::visit_block::<()>(&function.body, &mut |expression| {
            found |= matches!(expression, Expr::Call { name, .. } if name == LISTEN_TLS);
            Ok(())
        }).expect("Net TLS visitor cannot fail");
    }
    found
}

pub(super) fn collect_imports(module: &witchy_syntax::ast::Module) -> BTreeSet<&'static str> {
    let scope = witchy_canonical_source::scope::canonical_scope(module);
    let mut needed = BTreeSet::new();
    for item in &module.items {
        let Item::Function(function) = item else { continue };
        if !scope.contains(&function.name) { continue; }
        witchy_syntax::ast::visit::visit_block::<()>(&function.body, &mut |expression| {
            if let Expr::Call { name, .. } = expression && let Some((index, _, _)) = operation(name) {
                needed.insert(NET_IMPORT_NAMES[index]);
            }
            Ok(())
        }).expect("Net import visitor cannot fail");
    }
    needed
}

/// Whether the collected Net imports read any response.
pub(super) fn needs_responses(imports: &BTreeSet<&'static str>) -> bool {
    NET_IMPORT_NAMES.iter().enumerate().any(|(index, name)| answers_response(index) && imports.contains(name))
}

pub(super) fn catalog(catalog: CanonicalAbiCatalog, signatures: &mut Vec<FunctionSignature>, layout: witchy_wir::layout::LayoutId) -> Result<CanonicalAbiCatalog, CanonicalLoweringError> {
    let net = ValueType::Capability(NET_CAPABILITY_KIND);
    let listener = ValueType::Capability(LISTENER_CAPABILITY_KIND);
    let socket = ValueType::Capability(SOCKET_CAPABILITY_KIND);
    let response = ValueType::Capability(CAP_RESPONSE_KIND);
    let text = ValueType::Ref(layout);
    let shapes = [
        (vec![net, text], vec![response]),
        (vec![listener], vec![response]),
        (vec![socket, text], vec![response]),
        (vec![socket], vec![response]),
        (vec![socket, ValueType::Int], vec![response]),
        (vec![socket, text], vec![response]),
        (vec![socket], vec![]),
        (vec![listener], vec![]),
        (vec![socket], vec![response]),
        (vec![net, text], vec![response]),
        (vec![net, text], vec![response]),
    ];
    let mut ids = [FunctionSignatureId::new(0); 11];
    for (index, (parameters, results)) in shapes.into_iter().enumerate() {
        let id = FunctionSignatureId::new(u32::try_from(signatures.len()).map_err(|_| CanonicalLoweringError::IdOverflow)?);
        let parameter_effects = parameters.iter().enumerate().filter_map(|(index, ty)| {
            matches!(ty, ValueType::Ref(_)).then_some((index as u32, vec![MemoryEffect::Read]))
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
        use witchy_types::typeck::{ty_to_ast, Ty};
        if name == LISTEN_TLS { return self.lower_listen_tls(args).map(Some); }
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
        if matches!(index, 0 | 2 | 5 | 9 | 10) {
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
        if !answers_response(index) {
            self.push_operation(Operation::HostCall { destinations: Vec::new(), import, arguments });
            return Ok(None);
        }
        let response = self.fresh(ValueType::Capability(CAP_RESPONSE_KIND))?;
        self.push_operation(Operation::HostCall { destinations: vec![response], import, arguments });
        let checked = |ty: Ty| ty_to_ast(&ty).ok_or(CanonicalLoweringError::UnsupportedExpression("Net result type"));
        let value = match index {
            0 | 1 | 9 => {
                let (success, minted) = if index == 0 {
                    (checked(Ty::Listener)?, LISTENER_CAPABILITY_KIND)
                } else {
                    (checked(Ty::Socket)?, SOCKET_CAPABILITY_KIND)
                };
                self.lower_cap_result_with(response, success, |this, _empty| this.lower_response_take(response, minted))?
            }
            2 | 5 => self.lower_cap_result_with(response, checked(Ty::Unit)?, |this, _| this.push_zero(ValueType::Unit))?,
            10 => {
                let list = Type::Named("List".into(), vec![Type::Named("String".into(), Vec::new())]);
                self.lower_cap_result_with(response, list, |this, addresses| this.lower_listing(addresses))?
            }
            _ => self.lower_cap_result(response, "String")?,
        };
        Ok(Some(value))
    }

    /// `net.listen_tls(addr, cert_pem, key)` as `Result(Listener, CapError)`.
    fn lower_listen_tls(&mut self, args: &[Expr]) -> Result<ValueId, CanonicalLoweringError> {
        use witchy_types::typeck::{ty_to_ast, Ty};
        use witchy_wir::canonical_safewir::SECRET_CAPABILITY_KIND;
        if args.len() != 4 {
            return Err(CanonicalLoweringError::CallArityMismatch { callee: LISTEN_TLS.to_owned(), expected: 4, actual: args.len() });
        }
        let token = self.lower_expr(&args[0])?;
        if self.values[&token] != ValueType::Capability(NET_CAPABILITY_KIND) {
            return Err(CanonicalLoweringError::UnsupportedExpression("Net operation requires its authenticated receiver kind"));
        }
        let layout = self.string_layout_id()?;
        let address = self.lower_string_argument(&args[1])?;
        let address = self.string_slice_argument(address, layout)?;
        let certificate = self.lower_string_argument(&args[2])?;
        let certificate = self.string_slice_argument(certificate, layout)?;
        let key = self.lower_expr(&args[3])?;
        if self.values[&key] != ValueType::Capability(SECRET_CAPABILITY_KIND) {
            return Err(CanonicalLoweringError::UnsupportedExpression("TLS key must be an authenticated Secret"));
        }
        let import = self.capability_import(witchy_wir::canonical_safewir::NET_LISTEN_TLS_IMPORT)?;
        let response = self.fresh(ValueType::Capability(CAP_RESPONSE_KIND))?;
        self.push_operation(Operation::HostCall { destinations: vec![response], import, arguments: vec![
            HostCallArgument::Capability(token), address, certificate, HostCallArgument::Capability(key),
        ] });
        let success = ty_to_ast(&Ty::Listener).ok_or(CanonicalLoweringError::UnsupportedExpression("Net result type"))?;
        self.lower_cap_result_with(response, success, |this, _empty| this.lower_response_take(response, LISTENER_CAPABILITY_KIND))
    }
}
