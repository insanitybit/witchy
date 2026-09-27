//! Private call authority does not authenticate completion of a resource action.
//! Refuse erased bridge obligations until their transfer/disposition is admitted.

use std::collections::BTreeSet;
use witchy_canonical_source::storage::StoredObligationCatalog;
use witchy_syntax::ast::{Convention, Item, Module, Type};
use crate::runtime_type::{DeclarationKind, PackageSource, RuntimeDeclarationCatalog};

pub(crate) fn validate(
    module: &Module,
    runtime_functions: &[String],
    catalog: Option<&RuntimeDeclarationCatalog>,
) -> Result<(), String> {
    let runtime_functions = runtime_functions.iter().collect::<BTreeSet<_>>();
    let mut candidates = Vec::new();
    for item in &module.items {
        let Item::Function(function) = item else { continue };
        if !runtime_functions.contains(&function.name)
            || !function.params.iter().any(|parameter| parameter.convention == Convention::Own) {
            continue;
        }
        // Resolved names are lookup keys only. The bridge policy is selected
        // from the loader-authenticated declaration's logical identity.
        let identity = catalog.and_then(|catalog| catalog.resolve(&function.name, DeclarationKind::Function));
        let authenticated_bridge = identity.filter(|identity| {
            identity.package().source() == &PackageSource::Toolchain
                && identity.module().split_first().is_some_and(|(root, modules)| {
                    root == "std" && witchy_syntax::intrinsics::private_source_bridge_callers(
                        &format!("{}.{}", modules.join("."), identity.name()),
                    ).is_some()
                })
        });
        if authenticated_bridge.is_none() {
            // A loader-authenticated user declaration with a similar spelling
            // is ordinary source, not a privileged toolchain bridge.
            if identity.is_none_or(|identity| identity.package().source() == &PackageSource::Toolchain)
                && witchy_syntax::intrinsics::private_source_bridge_callers(&function.name).is_some() {
                return Err(format!("private source bridge `{}` lacks authenticated toolchain declaration identity", function.name));
            }
            continue;
        }
        candidates.push(function);
    }
    if candidates.is_empty() { return Ok(()); }
    let obligations = StoredObligationCatalog::from_module(module)
        .ok_or("private source bridge requires a checked stored-obligation catalog")?;
    let definitions = witchy_canonical_source::aggregates::type_definitions(module);
    for function in candidates {
        let consumes_must = function.params.iter().any(|parameter| {
            parameter.convention == Convention::Own
                && parameter.ty.as_ref().is_some_and(|ty| obligations.carries_must_source(ty, &definitions))
        });
        let result = function.ret.as_ref().cloned().unwrap_or(Type::Tuple(vec![]));
        if consumes_must && !obligations.carries_must_source(&result, &definitions) {
            // A carried result is merely a necessary condition, not proof of
            // correspondence to this input. No completed-disposition metadata
            // exists for an erased private bridge, even under immediate await.
            return Err(format!(
                "private source bridge `{}` erases a stored must-consume obligation without an admitted completion or transfer contract; deferred resource disposition is unsupported",
                function.name,
            ));
        }
    }
    Ok(())
}

#[doc(hidden)]
pub mod self_checks {
    use super::*;
    use crate::runtime_type::{ModuleLoadIdentity, PackageCoordinate};

    pub fn authenticated_identity_and_stored_obligations() {
        fn module(input: &str, name: &str) -> Module {
            let mut module = witchy_syntax::parser::parse_module(&format!(
                "must type Ticket:\n    Ticket(Int)\ntype Holder(a):\n    Holder(a)\ntype Phantom(a):\n    Phantom(Int)\nfn bridge(own value: {input}) -> Int:\n    0\n",
            )).unwrap();
            for item in &mut module.items {
                if let Item::Function(function) = item { function.name = name.into(); }
            }
            module
        }
        fn catalog(key: &str, source: PackageSource, namespace: &str) -> RuntimeDeclarationCatalog {
            let owner = ModuleLoadIdentity::new(
                PackageCoordinate::new(source, "test/package", "1").unwrap(), [namespace, "task"],
            ).unwrap();
            let mut catalog = RuntimeDeclarationCatalog::default();
            catalog.insert_resolved(key, &owner, "__handle_into_raw", DeclarationKind::Function).unwrap();
            catalog
        }
        // Compiler keys may be aliases; only the authenticated logical identity
        // selects bridge policy. The real loader includes the `std` namespace.
        let alias = "renamed.bridge";
        let live = vec![alias.into()];
        let toolchain = catalog(alias, PackageSource::Toolchain, "std");
        let nested = module("Holder(Ticket)", alias);
        assert!(validate(&nested, &live, Some(&toolchain)).unwrap_err().contains("erases a stored must-consume"));
        assert!(validate(&nested, &[], Some(&toolchain)).is_ok());
        for input in ["Phantom(Ticket)", "fn(Ticket) -> Int"] {
            assert!(validate(&module(input, alias), &live, Some(&toolchain)).is_ok());
        }
        let known = "task.__handle_into_raw";
        let named = module("Ticket", known);
        let live = vec![known.into()];
        assert!(validate(&named, &live, None).unwrap_err().contains("authenticated toolchain"));
        let wrong_namespace = catalog(known, PackageSource::Toolchain, "other");
        assert!(validate(&named, &live, Some(&wrong_namespace)).unwrap_err().contains("authenticated toolchain"));
        let user = catalog(known, PackageSource::Workspace, "std");
        assert!(validate(&named, &live, Some(&user)).is_ok());
    }
}
