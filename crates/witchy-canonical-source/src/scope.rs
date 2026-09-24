//! The canonical executable scope of a checked module: the declarations
//! reachable from its entry roots after monomorphization. One judgment shared
//! by the proper-tail manifest, the callable witness, the layouts, the
//! lowering and SafeWIR's service-site check, so their ordinals agree.

use std::collections::BTreeSet;

use witchy_syntax::ast::Module;

/// The canonical scope of a linked module: the entry scope (`main` and the
/// selected entry module's existing exported declarations) plus every function reachable
/// from it by name, transitively, through standard-library bodies and
/// monomorphized instances. Reachability is by exact declaration name after
/// monomorphization has renamed call sites to their instances, so a program
/// carries exactly the library code it references and nothing it does not; a
/// reachable body outside the compact subset still refuses precisely at
/// lowering. The scope is one judgment shared by the proper-tail manifest, the
/// callable witness, the layouts, and the lowering, so their ordinals agree.
pub fn canonical_scope(module: &Module) -> std::sync::Arc<BTreeSet<String>> {
    // No cache: a fingerprint over function names and statement counts once
    // served two different programs of the same shape the same scope (a
    // method call resolved to a std function in one and not the other), so
    // callers that need the scope for many lookups take the set once.
    let entry = module.linked_entry.as_deref();
    let functions = module
        .items
        .iter()
        .filter_map(|item| match item {
            witchy_syntax::ast::Item::Function(function) => Some((function.name.as_str(), function)),
            _ => None,
        })
        .collect::<std::collections::HashMap<_, _>>();
    let mut scope = BTreeSet::new();
    let entry_prefix = entry.map(|entry| format!("{entry}."));
    let mut pending = functions
        .keys()
        // Imported declarations are dependencies, not additional entry roots.
        // Keep the established entry-module export surface (including private
        // compiler recipe entries), and keep unlinked/library checks closed
        // over all their declarations. Neither types nor lowering support
        // participate in this decision.
        .filter(|name| entry_prefix.as_ref().is_none_or(|prefix|
            **name == "main" || name.starts_with(prefix)))
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    let seeds = pending.iter().cloned().collect::<BTreeSet<_>>();
    while let Some(name) = pending.pop() {
        let Some(function) = functions.get(name.as_str()) else { continue };
        // A generic template reached by name has no concrete body to lower;
        // only its monomorphized instances (which call sites name after
        // specialization) join the scope. The entry scope keeps its templates
        // as before, so its ordinals do not move.
        if !seeds.contains(&name) && function_has_free_signature_variables(function) {
            continue;
        }
        // A call named like a compiler intrinsic (`string.chars`, `list.push`)
        // resolves to the compiler, never to the standard-library wrapper of
        // the same name; that wrapper's body is not part of the program.
        if !seeds.contains(&name) && name == "vm.serve" { continue; }
        if !seeds.contains(&name)
            && matches!(
                witchy_syntax::intrinsics::lookup(&name),
                Some(spec)
                    if !matches!(
                        spec.lowering,
                        witchy_syntax::intrinsics::IntrinsicLowering::SourceFunction
                    )
            )
        {
            continue;
        }
        if !scope.insert(name.clone()) {
            continue;
        }
        let _: Result<(), ()> = witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
            let callee = match expression {
                witchy_syntax::ast::Expr::Call { name, .. } | witchy_syntax::ast::Expr::Var(name) => Some(name),
                _ => None,
            };
            if let Some(callee) = callee
                && functions.contains_key(callee.as_str())
                && !scope.contains(callee)
            {
                pending.push(callee.clone());
            }
            Ok(())
        });
    }
    std::sync::Arc::new(scope)
}

/// Generic templates are compile-time inputs to monomorphization, not runtime
/// call targets. Concrete generated instances have no free variables and stay
/// in the manifest, where the checks below require their exact checked body and
/// signature.
pub fn function_has_free_signature_variables(function: &witchy_syntax::ast::Function) -> bool {
    let mut variables = Vec::new();
    for parameter in &function.params {
        if let Some(ty) = &parameter.ty {
            witchy_syntax::ast::collect_type_vars(ty, &mut variables);
        }
    }
    if let Some(result) = &function.ret {
        witchy_syntax::ast::collect_type_vars(result, &mut variables);
    }
    !variables.is_empty()
}
