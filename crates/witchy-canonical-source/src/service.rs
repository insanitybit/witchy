//! Source-bound isolated service sites. No backend integer selects a callback.
use witchy_syntax::ast::{Convention, Expr, Item, Type};
use crate::abi::CanonicalTargetPolicy;
use crate::source::CanonicalSourceView;

#[derive(Debug, Clone, PartialEq)]
pub struct ServiceSite<'a> {
    id: u32,
    source: [u8; 32],
    caller: u32,
    occurrence: u32,
    callback: u32,
    expression: &'a Expr,
}
impl<'a> ServiceSite<'a> {
    pub fn id(&self) -> u32 { self.id }
    pub fn source_digest(&self) -> [u8; 32] { self.source }
    pub fn caller_ordinal(&self) -> u32 { self.caller }
    pub fn occurrence(&self) -> u32 { self.occurrence }
    pub fn callback_ordinal(&self) -> u32 { self.callback }
    pub fn expression(&self) -> &'a Expr { self.expression }
}
fn closed_ordinal(source: &CanonicalSourceView<'_>, name: &str) -> Result<u32, String> {
    match source.closed_callable_abi(name).map(|abi| &abi.target_policy) {
        Some(CanonicalTargetPolicy::ClosedLocal(target)) => Ok(target.function_ordinal),
        _ => Err("service requires an exact checked declaration binding".into()),
    }
}
pub fn service_sites<'s>(source: &CanonicalSourceView<'s>) -> Result<Vec<ServiceSite<'s>>, String> {
    let module = source.module();
    let scope = crate::scope::canonical_scope(module);
    let mut sites = Vec::new();
    for item in &module.items {
        let Item::Function(function) = item else { continue };
        if !scope.contains(&function.name) { continue; }
        let mut occurrence = 0u32;
        witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
            if matches!(expression, Expr::Var(name) if name == "vm.serve") {
                return Err("vm.serve requires a direct checked source site, not a function value".into());
            }
            // A closure has a separate synthesized caller identity. Until that
            // identity is represented by this manifest, do not misbind it to
            // the containing declaration.
            if let Expr::Lambda { body, .. } = expression {
                witchy_syntax::ast::visit::visit_block(body, &mut |nested| {
                    if matches!(nested, Expr::Call { name, .. } if name == "vm.serve") {
                        Err("vm.serve inside a lambda requires a closure caller site".to_owned())
                    } else { Ok(()) }
                })?;
            }
            let Expr::Call { name, args } = expression else { return Ok::<_, String>(()) };
            if name != "vm.serve" { return Ok(()) }
            let [_, _, Expr::Var(callback)] = args.as_slice() else {
                return Err("service requires a bare callback declaration".into());
            };
            // Source checking independently rejects every lexical/local alias,
            // including a local that shadows a same-named global declaration.
            let is_bytes = |ty: &Type| matches!(ty.unqualified(), Type::Named(name, args)
                if name == "Bytes" && args.is_empty());
            let Some(Type::Fn(parameters, result, conventions, _)) = source.callable_signature(callback)
                .map(Type::unqualified) else {
                return Err("service callback has no checked callable signature".into());
            };
            if parameters.len() != 2 || !parameters.iter().all(is_bytes) || !is_bytes(result)
                || conventions.iter().any(|c| !matches!(c, Convention::Let | Convention::Own))
            {
                return Err("service callback requires default/own Bytes, Bytes -> Bytes".into());
            }
            sites.push(ServiceSite {
                id: u32::try_from(sites.len()).map_err(|_| "service site overflow")?,
                source: source.source_witness().source_bundle_digest(),
                caller: closed_ordinal(source, &function.name)?, occurrence,
                callback: closed_ordinal(source, callback)?, expression,
            });
            occurrence = occurrence.checked_add(1).ok_or("service occurrence overflow")?;
            Ok(())
        })?;
    }
    Ok(sites)
}
