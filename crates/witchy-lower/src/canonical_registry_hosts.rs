//! Checked registry capability calls over bounded host slices, without raw WIR.
use super::*;
use witchy_syntax::intrinsics::{self, IntrinsicId};
use witchy_wir::canonical_safewir::{SECRET_CAPABILITY_KIND, SECRET_STORE_CAPABILITY_KIND};

pub(super) const REQUIRED_SECRET_MESSAGE_PARTS: [&str; 2] = ["required secret `", "` was not granted"];

fn host_import(name: &str) -> Option<&'static str> {
    match intrinsics::lookup(name)?.id {
        IntrinsicId::SecretStoreGet | IntrinsicId::SecretStoreRequire => Some("secretstore_lookup_at"),
        IntrinsicId::CryptoSign => Some("crypto_sign_into"),
        IntrinsicId::CryptoPublicKey => Some("crypto_public_key_into"),
        IntrinsicId::CryptoReveal => Some("secret_reveal_at"),
        _ => None,
    }
}

pub(super) fn is_intrinsic(name: &str) -> bool { host_import(name).is_some() }
pub(super) fn is_require(name: &str) -> bool {
    intrinsics::lookup(name).is_some_and(|spec| spec.id == IntrinsicId::SecretStoreRequire)
}

pub(super) fn collect_imports(module: &witchy_syntax::ast::Module) -> (BTreeSet<&'static str>, bool) {
    let scope = witchy_canonical_source::scope::canonical_scope(module);
    let mut imports = BTreeSet::new();
    let mut requires_secret = false;
    for item in &module.items {
        let Item::Function(function) = item else { continue };
        if !scope.contains(&function.name) { continue; }
        witchy_syntax::ast::visit::visit_block::<()>(&function.body, &mut |expression| {
            if let Expr::Call { name, .. } = expression && let Some(import) = host_import(name) {
                imports.insert(import);
                if import == "secret_reveal_at" {
                    imports.extend(["cap_response_status", "cap_response_size", "cap_response_into"]);
                }
                requires_secret |= is_require(name);
            }
            Ok(())
        }).expect("registry import visitor cannot fail");
    }
    (imports, requires_secret)
}

impl FunctionLowerer<'_> {
    pub(super) fn lower_registry_intrinsic(
        &mut self, expression: &Expr, name: &str, arguments: &[Expr],
    ) -> Result<ValueId, CanonicalLoweringError> {
        let spec = intrinsics::lookup(name).filter(|_| is_intrinsic(name))
            .ok_or_else(|| CanonicalLoweringError::MissingBinding(name.to_owned()))?;
        let expected = if matches!(spec.id, IntrinsicId::CryptoPublicKey | IntrinsicId::CryptoReveal) { 1 } else { 2 };
        if arguments.len() != expected {
            return Err(CanonicalLoweringError::CallArityMismatch {
                callee: name.to_owned(), expected, actual: arguments.len(),
            });
        }
        // The first argument is evaluated before the message/name expression.
        // It remains an opaque value, never a guest-owned secret allocation.
        let token = self.lower_expr(&arguments[0])?;
        let is_store = matches!(spec.id, IntrinsicId::SecretStoreGet | IntrinsicId::SecretStoreRequire);
        let kind = if is_store { SECRET_STORE_CAPABILITY_KIND } else { SECRET_CAPABILITY_KIND };
        if self.values.get(&token) != Some(&ValueType::Capability(kind)) {
            return Err(CanonicalLoweringError::UnsupportedExpression("registry capability kind"));
        }
        if spec.id == IntrinsicId::CryptoReveal {
            // One host read answers `Result(String, CapError)`: a host-sealed
            // secret or the signing key is `Denied` (RFC-0126 rule 3).
            let response = self.fresh(ValueType::Capability(witchy_wir::canonical_safewir::CAP_RESPONSE_KIND))?;
            let import = self.capability_import("secret_reveal_at")?;
            self.push_operation(Operation::HostCall { destinations: vec![response], import,
                arguments: vec![HostCallArgument::Capability(token)] });
            return self.lower_cap_result(response, "String");
        }
        let layout = self.string_layout_id()?;
        if is_store {
            let text = self.lower_string_argument(&arguments[1])?;
            let text = self.clone_reference_token(text)?;
            let name_slice = self.string_slice_argument(text, layout)?;
            let import = self.capability_import("secretstore_lookup_at")?;
            let optional = self.fresh(ValueType::NullableCapability(SECRET_CAPABILITY_KIND))?;
            self.push_operation(Operation::HostCall { destinations: vec![optional], import,
                arguments: vec![HostCallArgument::Capability(token), name_slice] });
            if spec.id == IntrinsicId::SecretStoreGet { return Ok(optional); }

            let present = self.fresh(ValueType::Bool)?;
            self.push_operation(Operation::CapabilityIsPresent { destination: present, source: optional });
            let ready = self.new_block(&[])?;
            let missing = self.new_block(&[])?;
            let facts = self.snapshot_facts();
            self.finish_current(Terminator::BranchIf { condition: present,
                then_target: ready, then_arguments: Vec::new(),
                else_target: missing, else_arguments: Vec::new() })?;
            self.current = missing;
            let prefix = self.lower_string_literal(REQUIRED_SECRET_MESSAGE_PARTS[0])?;
            let suffix = self.lower_string_literal(REQUIRED_SECRET_MESSAGE_PARTS[1])?;
            let message = self.concat_string_values(prefix, text)?;
            let message = self.concat_string_values(message, suffix)?;
            let site = self.diagnostic_site(expression)?;
            self.abort_value_at(&site, message)?;
            self.restore_facts(facts, ready);
            self.current = ready;
            let secret = self.fresh(ValueType::Capability(SECRET_CAPABILITY_KIND))?;
            self.push_operation(Operation::CapabilityUnwrap { destination: secret, source: optional });
            return Ok(secret);
        }

        let mut inputs = vec![HostCallArgument::Capability(token)];
        let capacity = if spec.id == IntrinsicId::CryptoSign {
            let text = self.lower_string_argument(&arguments[1])?;
            let text = self.clone_reference_token(text)?;
            inputs.push(self.string_slice_argument(text, layout)?);
            128
        } else { 64 };
        let capacity = self.push_const_size(capacity)?;
        let zero = self.push_const_size(0)?;
        let output = self.alloc_bytes(zero, capacity)?;
        inputs.push(HostCallArgument::Buffer {
            place: witchy_wir::canonical_safewir::Place::projected(output,
                Projection::Slice { start: zero, length: capacity }, ValueType::Ref(layout), AccessMode::Write),
            extent: HostBufferExtent::Slice,
        });
        let required = self.fresh(ValueType::Size)?;
        let import = self.capability_import(host_import(name).expect("selected registry intrinsic"))?;
        self.push_operation(Operation::HostCall { destinations: vec![required], import, arguments: inputs });
        // Short output is not success: only the checked initialized prefix is
        // published, after the required size is bounded by this allocation.
        let fits = self.push_compare(ComparisonOperator::LessEqualUnsigned, required, capacity)?;
        self.push_guard(fits, witchy_wir::canonical_safewir::GuardKind::Bounds);
        self.push_operation(Operation::UpdateCount { object: output, count: required });
        self.string_lengths.insert(output, required);
        self.string_capacities.insert(output, capacity);
        Ok(output)
    }
}
