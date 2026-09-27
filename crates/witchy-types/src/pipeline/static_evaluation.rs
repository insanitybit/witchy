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

const RECIPE_VERSION: u32 = 3;

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
    sources: Vec<(String, Vec<u8>)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: &HashSet<String>,
    owners: AuthenticatedModuleOwners,
) -> Result<CheckedStaticEvaluationRecipe, PipelineError> {
    link_static_evaluation_recipe(sources, entry, expand, user_modules, owners, None, None)
}

/// Add the fixed, compiler-owned `main` that emits one reflected Site value.
/// Only the exact loader-owned source bundle can mint the resulting witness.
pub fn link_checked_static_site_output_from_source_bundle(
    sources: Vec<(String, Vec<u8>)>,
    entry: &str,
    expand: ComptimeExpander,
    user_modules: &HashSet<String>,
    owners: AuthenticatedModuleOwners,
    has_content: bool,
) -> Result<CheckedStaticEvaluationRecipe, PipelineError> {
    link_static_evaluation_recipe(sources, entry, expand, user_modules, owners, Some(has_content), None)
}

/// A compiler-owned fresh-island entry built from the same loader snapshot as
/// the checked Site. The selected callable names come from checked island
/// metadata; this recipe still resolves and checks the complete destination.
pub fn link_checked_static_island_output_from_source_bundle(
    sources: Vec<(String, Vec<u8>)>, entry: &str, expand: ComptimeExpander,
    user_modules: &HashSet<String>, owners: AuthenticatedModuleOwners,
    initial: &str, view: &str,
) -> Result<CheckedStaticEvaluationRecipe, PipelineError> {
    link_static_evaluation_recipe(sources, entry, expand, user_modules, owners,
        None, Some((initial, view)))
}

fn source_callable_name(name: &str) -> bool {
    !name.is_empty() && name.split('.').all(|part| {
        let mut bytes = part.bytes();
        bytes.next().is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

fn link_static_evaluation_recipe(
    mut sources: Vec<(String, Vec<u8>)>, entry: &str, expand: ComptimeExpander,
    user_modules: &HashSet<String>, owners: AuthenticatedModuleOwners,
    output_content: Option<bool>, fresh_island: Option<(&str, &str)>,
) -> Result<CheckedStaticEvaluationRecipe, PipelineError> {
    if output_content.is_some() && fresh_island.is_some() {
        return Err(refused("static recipe cannot select two output adapters"));
    }
    if fresh_island.is_some_and(|(initial, view)|
        !source_callable_name(initial) || !source_callable_name(view)) {
        return Err(refused("fresh-island callable name is not a source declaration"));
    }
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
    if let Some(has_content) = output_content {
        let (_, selected) = modules.iter_mut().find(|(name, _)| name == entry)
            .ok_or_else(|| refused("static output entry is missing"))?;
        if selected.items.iter().any(|item| matches!(item, Item::Function(function) if function.name == "main")) {
            return Err(refused("static output recipe requires an entry without `main`"));
        }
        let adapter = if has_content {
            "fn main(console: Console, args: List(String)):\n    console.print(glamour.static_site_json(web(glamour.static_content_from_args(args))))\n"
        } else {
            "fn main(console: Console):\n    console.print(glamour.static_site_json(web()))\n"
        };
        let generated = witchy_syntax::parser::parse_module(adapter)
            .map_err(|error| refused(format!("invalid compiler static output adapter: {error}")))?;
        selected.item_lines.extend(std::iter::repeat_n(0, generated.items.len()));
        selected.items.extend(generated.items);
    }
    if let Some((initial, view)) = fresh_island {
        let (_, glamour) = modules.iter_mut().find(|(name, _)| name == "glamour")
            .expect("authenticated toolchain Glamour retained");
        let helpers = witchy_syntax::parser::parse_module(
            "pub fn compiler_fresh_start(route: String) -> Start:\n    Start(route, \"\")\npub fn compiler_fresh_graph(key: String, rendered: Ui(msg)) -> String:\n    json.stringify(client_region_live_graph(key, rendered))\n",
        ).map_err(|error| refused(format!("invalid compiler fresh-island helpers: {error}")))?;
        glamour.item_lines.extend(std::iter::repeat_n(0, helpers.items.len()));
        glamour.items.extend(helpers.items);
        let (_, selected) = modules.iter_mut().find(|(name, _)| name == entry)
            .expect("raw entry retained");
        if selected.items.iter().any(|item| matches!(item, Item::Function(function) if function.name == "main")) {
            return Err(refused("fresh-island recipe requires an entry without `main`"));
        }
        let adapter = format!(
            "fn main(console: Console, args: List(String)):\n    console.print(glamour.compiler_fresh_graph(args.at(1), {view}({initial}(glamour.compiler_fresh_start(args.at(0))))))\n"
        );
        let generated = witchy_syntax::parser::parse_module(&adapter)
            .map_err(|error| refused(format!("invalid compiler fresh-island adapter: {error}")))?;
        selected.item_lines.extend(std::iter::repeat_n(0, generated.items.len()));
        selected.items.extend(generated.items);
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"witchy.checked-static-evaluation-recipe.v1");
    hasher.update(&RECIPE_VERSION.to_le_bytes());
    hasher.update(&[if fresh_island.is_some() { 3 } else { match output_content { None => 0, Some(false) => 1, Some(true) => 2 } }]);
    if let Some((initial, view)) = fresh_island {
        hash_text(&mut hasher, initial);
        hash_text(&mut hasher, view);
    }
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

pub(super) fn hash_owner_assignment(
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
