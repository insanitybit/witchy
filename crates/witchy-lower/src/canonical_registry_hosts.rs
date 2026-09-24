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
        let expected = if spec.id == IntrinsicId::CryptoPublicKey { 1 } else { 2 };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn checked(source: &str) -> witchy_types::pipeline::CheckedModule {
        // Test inputs are the actual std sources, not synthetic intrinsic
        // declarations or copied witnesses. No host execution occurs here.
        let root = std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR for tests"),
        )
        .join("../../std");
        let mut pending = vec![("registry_probe".to_owned(), source.to_owned())];
        let mut seen = BTreeSet::new();
        let mut sources = Vec::new();
        while let Some((name, text)) = pending.pop() {
            if !seen.insert(name.clone()) { continue; }
            let module = witchy_syntax::parser::parse_module(&text).expect("parse fixture input");
            for import in module.imports {
                let text = std::fs::read_to_string(root.join(format!("{import}.witchy"))).expect("actual std dependency");
                pending.push((import, text));
            }
            sources.push((name, text.into_bytes()));
        }
        witchy_types::pipeline::link_checked_from_source_bundle(sources, "registry_probe",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default())).expect("fresh checked registry source")
    }

    const SOURCE: &str = r#"import crypto
import secretstore
fn main(clock: Clock, root: Dir, store: SecretStore, key: Secret[Seal]) -> Int:
    root.make_dir("nested")
    let lookup = fn(): store.get("signing")
    match lookup():
        Some(found) ->
            let sign = fn(message: String): crypto.sign(found, message)
            let first = sign("message")
            let second = crypto.public_key(key)
            clock.now() + first.length() + second.length()
        None -> clock.now()
"#;

    #[test]
    fn registry_hosts_source_capture_nullable_signing_reaches_admission() {
        let checked = checked(SOURCE);
        let validated = crate::canonical_pipeline::validate_checked_module(&checked).expect("initial and final validation");
        let source = checked.canonical_source().unwrap();
        let layouts = crate::canonical_layouts::derive_layout_bundle(source).unwrap();
        let module = lower_typed_module(source, layouts).expect("inspect untrusted lowered candidate");
        for name in witchy_wir::canonical_safewir::REGISTRY_HOST_IMPORT_NAMES {
            assert!(module.imports().iter().any(|import| import.name == name), "missing {name}");
        }
        assert!(module.imports().iter().all(|import| !matches!(import.name.as_str(),
            "now" | "dir_make_dir" | "secretstore_lookup" | "crypto.sign" | "crypto.public_key")));
        assert!(!module.owner_families().is_empty(), "captures keep their authenticated owner bindings");
        let source = checked.canonical_source().unwrap();
        witchy_wir::admit_canonical(validated, &source.view()).expect("independent plan and encoded bytes admit");
    }

    #[test]
    fn registry_hosts_signing_publishes_only_guarded_returned_prefix() {
        let checked = checked(SOURCE);
        crate::canonical_pipeline::validate_checked_module(&checked).expect("checked registry buffers");
        let source = checked.canonical_source().unwrap();
        let layouts = crate::canonical_layouts::derive_layout_bundle(source).unwrap();
        let module = lower_typed_module(source, layouts).expect("inspect untrusted lowered candidate");
        let signing: BTreeSet<_> = module.imports().iter().filter(|import|
            matches!(import.name.as_str(), "crypto_sign_into" | "crypto_public_key_into"))
            .map(|import| import.id).collect();
        let mut inspected = 0;
        for function in module.functions() {
            for block in &function.blocks {
                for (index, operation) in block.operations.iter().enumerate() {
                    let Operation::HostCall { destinations, import, arguments } = operation else { continue };
                    if !signing.contains(import) { continue; }
                    let HostCallArgument::Buffer { place, .. } = arguments.last().unwrap() else { panic!("bounded output"); };
                    let object = place.object();
                    let required = destinations[0];
                    let tail = &block.operations[index + 1..];
                    let publish = tail.iter().position(|operation| matches!(operation,
                        Operation::UpdateCount { object: actual, count } if *actual == object && *count == required)).expect("publish returned prefix");
                    assert!(tail[..publish].iter().any(|operation| matches!(operation,
                        Operation::Guard { kind: witchy_wir::canonical_safewir::GuardKind::Bounds, .. })));
                    inspected += 1;
                }
            }
        }
        assert_eq!(inspected, 2);
    }

    #[test]
    fn registry_hosts_require_keeps_eager_source_diagnostic() {
        let source = r#"import secretstore
fn rename(var name: String) -> String:
    name = "changed"
    "missing"
fn main(store: SecretStore):
    var name = "signing"
    let ignored = store.require(rename(name))
"#;
        let checked = checked(source);
        let artifact = crate::canonical_pipeline::admit_checked_module(&checked).expect("require admits with checked abort source site");
        assert!(artifact.wasm().windows("secretstore_lookup_at".len()).any(|bytes| bytes == b"secretstore_lookup_at"));
        assert!(artifact.wasm().windows("__witchy_abort_at".len()).any(|bytes| bytes == b"__witchy_abort_at"));
    }

    #[test]
    fn registry_hosts_clock_does_not_select_monotonic_or_other_capabilities() {
        let checked = checked("fn main(clock: Clock) -> Int:\n    clock.now()\n");
        crate::canonical_pipeline::validate_checked_module(&checked).expect("clock source admits");
        let source = checked.canonical_source().unwrap();
        let layouts = crate::canonical_layouts::derive_layout_bundle(source).unwrap();
        let module = lower_typed_module(source, layouts).expect("inspect untrusted lowered candidate");
        assert!(module.imports().iter().any(|import| import.name == "clock_now"));
        assert!(!module.imports().iter().any(|import| import.name == "now_monotonic"));
        let wrong = "fn main(console: Console) -> Int:\n    console.now()\n";
        assert!(witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("wrong".into(), wrong.as_bytes().to_vec())], "wrong",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default())).is_err());
    }
}
