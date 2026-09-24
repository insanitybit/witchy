//! Authenticated source recipe for compiler-owned Glamour static evaluation.
//!
//! The origin-bearing constructors are inserted while the loader-owned source
//! bundle is still intact.  The ordinary resolver and semantic checker then
//! create one fresh typed/fact product for the transformed program; a linked
//! AST is never rechecked to manufacture this authority.

use super::*;
use crate::runtime_type::{DeclarationIdentity, ModuleLoadIdentity, PackageSource};
use sha2::{Digest, Sha256};
use witchy_syntax::ast::{Expr, Item, Module};

const RECIPE_VERSION: u32 = 1;

#[derive(Debug)]
pub struct CheckedStaticEvaluationRecipe {
    checked: CheckedModule,
}

impl CheckedStaticEvaluationRecipe {
    pub fn checked(&self) -> &CheckedModule { &self.checked }
    pub fn canonical_source(&self) -> &CanonicalCheckedSource {
        self.checked.canonical_source().expect("static evaluation recipe retains a canonical source")
    }
}

fn refused(message: impl Into<String>) -> PipelineError {
    PipelineError::Type(TypeError { message: message.into() })
}

/// Link a compiler-owned static-evaluation source recipe from the exact raw
/// bundle authenticated by the loader.  The resulting witness binds the
/// original bytes and module owners plus this fixed rewrite version; callers
/// cannot substitute a transformed AST or claim a digest.
pub fn link_checked_static_evaluation_from_source_bundle(
    mut sources: Vec<(String, Vec<u8>)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: &HashSet<String>,
    owners: AuthenticatedModuleOwners,
) -> Result<CheckedStaticEvaluationRecipe, PipelineError> {
    sources.sort_by(|left, right| left.0.cmp(&right.0));
    if sources.iter().any(|(name, _)| name.is_empty())
        || sources.windows(2).any(|pair| pair[0].0 == pair[1].0)
        || !sources.iter().any(|(name, _)| name == entry)
    {
        return Err(refused("static evaluation recipe requires unique raw module names and its entry"));
    }
    let original = checked_source_digest(&sources, entry, &PrecompiledDeclarationEnvironment::default());
    let glamour = owners.owner("glamour").ok_or_else(|| {
        refused("static evaluation recipe lacks authenticated toolchain Glamour source")
    })?;
    if !is_toolchain_glamour(glamour) {
        return Err(refused(
            "static evaluation recipe requires `glamour` to be owned by toolchain witchy/glamour",
        ));
    }
    let mut modules = sources.iter().map(|(name, bytes)| {
        let source = std::str::from_utf8(bytes)
            .map_err(|error| refused(format!("static evaluation source `{name}` is not UTF-8: {error}")))?;
        let module = witchy_syntax::parser::parse_module(source)
            .map_err(|error| refused(format!("static evaluation source `{name}` failed to parse: {error}")))?;
        Ok((name.clone(), module))
    }).collect::<Result<Vec<_>, PipelineError>>()?;
    for (name, module) in &mut modules {
        reject_source_spelled_origin_constructor(name, module)?;
        let owner = owners.owner(name).ok_or_else(|| refused(format!(
            "static evaluation recipe lacks authenticated owner for source module `{name}`"
        )))?;
        rewrite_module(module, owner)?;
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"witchy.checked-static-evaluation-recipe.v1");
    hasher.update(&RECIPE_VERSION.to_le_bytes());
    hasher.update(&original);
    hasher.update(&(entry.len() as u64).to_le_bytes());
    hasher.update(entry.as_bytes());
    for (module, identity) in owners.assignments() {
        hash_owner_assignment(&mut hasher, module, identity);
    }
    let derived = *hasher.finalize().as_bytes();
    let (checked, typed) = link_checked_with(
        modules, entry, expand, LinkMode::StaticEvaluation, Some(user_modules), true, Some(owners),
    )?;
    let typed = typed.ok_or_else(|| refused("static evaluation recipe link did not retain typed source"))?;
    let checked = finish_canonical_source(
        checked, typed, derived, PrecompiledDeclarationEnvironment::default(),
    )?;
    Ok(CheckedStaticEvaluationRecipe { checked })
}

fn reject_source_spelled_origin_constructor(
    module_name: &str,
    module: &Module,
) -> Result<(), PipelineError> {
    if module_name == "glamour" {
        return Ok(());
    }
    witchy_syntax::ast::visit::visit_module_exprs::<PipelineError>(module, &mut |expression| {
        let constructor: Option<&str> = match expression {
            Expr::Call { name, .. } => name.strip_prefix("glamour."),
            Expr::MethodCall { receiver, method, .. }
                if matches!(receiver.as_ref(), Expr::Var(namespace) if namespace == "glamour") =>
            Some(method.as_str()),
            _ => None,
        };
        if matches!(constructor, Some("interactive_with_origin" | "client_region_with_origin")) {
            return Err(refused(
                "Glamour origin constructors are compiler-owned and cannot appear in static-evaluation source",
            ));
        }
        Ok(())
    })
}

fn is_toolchain_glamour(identity: &ModuleLoadIdentity) -> bool {
    identity.package().source() == &PackageSource::Toolchain
        && identity.package().name() == "witchy/glamour"
        && identity.module_path() == ["src", "glamour"]
}

fn hash_owner_assignment(
    hasher: &mut blake3::Hasher,
    module: &str,
    identity: &ModuleLoadIdentity,
) {
    hash_text(hasher, module);
    match identity.package().source() {
        PackageSource::Toolchain => {
            hasher.update(&[0]);
        }
        PackageSource::Workspace => {
            hasher.update(&[1]);
        }
        PackageSource::Registry(registry) => {
            hasher.update(&[2]);
            hash_text(hasher, registry);
        }
    }
    hash_text(hasher, identity.package().name());
    hash_text(hasher, identity.package().version());
    hasher.update(&(identity.module_path().len() as u64).to_le_bytes());
    for segment in identity.module_path() {
        hash_text(hasher, segment);
    }
}

fn hash_text(hasher: &mut blake3::Hasher, text: &str) {
    hasher.update(&(text.len() as u64).to_le_bytes());
    hasher.update(text.as_bytes());
}

fn rewrite_module(module: &mut Module, owner: &ModuleLoadIdentity) -> Result<(), PipelineError> {
    for item in &mut module.items {
        let Item::Function(function) = item else { continue; };
        let identity = owner.declaration(DeclarationKind::Function, &function.name)
            .map_err(|error| refused(format!("static evaluation function identity: {error}")))?;
        let mut ordinal = 0_usize;
        witchy_syntax::ast::visit::visit_block_mut(&mut function.body, &mut |expression| {
            let constructor = match expression {
                Expr::Call { name, .. } => match name.as_str() {
                    "glamour.interactive" => Some("interactive"),
                    "glamour.client_region" => Some("client_region"),
                    _ => None,
                },
                Expr::MethodCall { receiver, method, .. }
                    if matches!(receiver.as_ref(), Expr::Var(namespace) if namespace == "glamour") =>
                match method.as_str() {
                    "interactive" => Some("interactive"),
                    "client_region" => Some("client_region"),
                    _ => None,
                },
                _ => None,
            };
            let Some(constructor) = constructor else { return Ok(()); };
            let args = match expression {
                Expr::Call { args, .. } => args,
                Expr::MethodCall { args, .. } => args,
                _ => unreachable!("static constructor matched above"),
            };
            if args.len() != 2 {
                return Err(refused(format!(
                    "`glamour.{constructor}` must use its checked two-argument positional form"
                )));
            }
            let source_identity = interactive_source_identity(&identity, ordinal, constructor);
            ordinal += 1;
            args.insert(0, Expr::Str(source_identity));
            match expression {
                Expr::Call { name, .. } => *name = format!("glamour.{constructor}_with_origin"),
                Expr::MethodCall { args, .. } => {
                    let args = std::mem::take(args);
                    *expression = Expr::Call { name: format!("glamour.{constructor}_with_origin"), args };
                }
                _ => unreachable!("static constructor matched above"),
            }
            Ok(())
        })?;
    }
    Ok(())
}

fn interactive_source_identity(owner: &DeclarationIdentity, ordinal: usize, constructor: &str) -> String {
    let material = format!(
        "witchy.glamour.interactive-origin.v1|owner={:?}:{}@{}:{:?}:{}|structural-ordinal={ordinal}|constructor={constructor}",
        owner.package().source(), owner.package().name(), owner.package().version(), owner.module(), owner.name(),
    );
    let digest = Sha256::digest(material.as_bytes());
    let mut text = String::with_capacity(24 + digest.len() * 2);
    text.push_str("interactive-origin1-");
    for byte in digest { text.push_str(&format!("{byte:02x}")); }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_type::{PackageCoordinate, PackageSource};

    fn no_expand(
        _name: &str,
        _module: &mut Module,
        _siblings: &[(String, Module)],
    ) -> Result<witchy_syntax::origin::OriginTable, String> {
        Ok(witchy_syntax::origin::OriginTable::default())
    }

    fn owner() -> ModuleLoadIdentity {
        ModuleLoadIdentity::new(
            PackageCoordinate::new(PackageSource::Workspace, "example/static", "0.1.0").unwrap(),
            ["src", "main"],
        ).unwrap()
    }

    #[test]
    fn recipe_witness_binds_every_authenticated_owner_assignment() {
        let sources = vec![("main".to_string(), b"fn main() -> ():\n    ()\n".to_vec())];
        let owner_set = |main_owner| {
            let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/stdlib", "0.1.0").unwrap();
            let glamour = PackageCoordinate::new(PackageSource::Toolchain, "witchy/glamour", "0.1.0").unwrap();
            let mut assignments = vec![("main".to_string(), main_owner)];
            assignments.push(("glamour".to_string(), ModuleLoadIdentity::new(
                glamour, ["src", "glamour"],
            ).unwrap()));
            assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| (
                (*name).to_string(), ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).unwrap(),
            )));
            AuthenticatedModuleOwners::from_loader_assignments(assignments).unwrap()
        };
        let original = link_checked_static_evaluation_from_source_bundle(
            sources.clone(), "main", no_expand, &HashSet::from(["main".to_string()]), owner_set(owner()),
        ).unwrap();
        let substituted_owner = ModuleLoadIdentity::new(
            PackageCoordinate::new(PackageSource::Workspace, "example/static", "0.2.0").unwrap(),
            ["src", "main"],
        ).unwrap();
        let substituted = link_checked_static_evaluation_from_source_bundle(
            sources, "main", no_expand, &HashSet::from(["main".to_string()]), owner_set(substituted_owner),
        ).unwrap();
        assert_ne!(
            original.canonical_source().source_witness().source_bundle_digest(),
            substituted.canonical_source().source_witness().source_bundle_digest(),
        );
    }

    #[test]
    fn recipe_rejects_source_spelled_origin_constructors() {
        let sources = vec![(
            "main".to_string(),
            b"import glamour\n\nfn main() -> ():\n    glamour.interactive_with_origin(\"forged\", (), ())\n".to_vec(),
        )];
        let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/stdlib", "0.1.0").unwrap();
        let glamour = PackageCoordinate::new(PackageSource::Toolchain, "witchy/glamour", "0.1.0").unwrap();
        let mut assignments = vec![("main".to_string(), owner())];
        assignments.push(("glamour".to_string(), ModuleLoadIdentity::new(
            glamour, ["src", "glamour"],
        ).unwrap()));
        assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| (
            (*name).to_string(), ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).unwrap(),
        )));
        let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments).unwrap();
        let error = link_checked_static_evaluation_from_source_bundle(
            sources, "main", no_expand, &HashSet::from(["main".to_string()]), owners,
        ).expect_err("source cannot claim a compiler origin");
        assert!(error.to_string().contains("compiler-owned"), "{error}");
    }
}
