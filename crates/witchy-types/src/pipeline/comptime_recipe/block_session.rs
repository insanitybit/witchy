//! Source-selected compiler entry. No caller-supplied AST or prior output can
//! enter this session. Later blocks need an authenticated output receipt.
use super::*;
use witchy_syntax::ast::{Expr, Function, Stmt};

const REWRITE_VERSION: u32 = 1;

/// Compiler-owned immutable source session for the first original block.
/// This is an explicit recipe lane, not the default expansion implementation.
/// Its private fields contain only parsed raw bytes and loader assignments;
/// there is deliberately no mutable module accessor or output ingestion API.
pub struct ComptimeSourceSession {
    original_digest: [u8; 32],
    modules: Vec<(String, Module)>,
    selected: String,
    owners: AuthenticatedModuleOwners,
    user_modules: HashSet<String>,
}

fn refuse(message: impl Into<String>) -> PipelineError {
    PipelineError::Type(TypeError { message: message.into() })
}

impl ComptimeSourceSession {
    pub fn from_source_bundle(
        mut sources: Vec<(String, Vec<u8>)>, selected: &str,
        user_modules: HashSet<String>, owners: AuthenticatedModuleOwners,
    ) -> Result<Self, PipelineError> {
        sources.sort_by(|a, b| a.0.cmp(&b.0));
        if sources.iter().any(|(name, _)| name.is_empty())
            || sources.windows(2).any(|pair| pair[0].0 == pair[1].0)
            || !sources.iter().any(|(name, _)| name == selected)
            || (selected != "comptime" && sources.iter().any(|(name, _)| name == "comptime"))
        { return Err(refuse("comptime session requires unique raw sources and an unambiguous selected module")); }
        authenticate_meta(&owners)?;
        owners.validate_module_names(sources.iter().map(|(name, _)| name.clone()))?;
        if !user_modules.contains(selected) {
            return Err(refuse("comptime session selected module is not a loader-owned user module"));
        }
        let original_digest = checked_source_digest(&sources, selected, &PrecompiledDeclarationEnvironment::default());
        let modules = sources.into_iter().map(|(name, bytes)| {
            let source = std::str::from_utf8(&bytes).map_err(|error| refuse(format!("comptime source `{name}` is not UTF-8: {error}")))?;
            let module = witchy_syntax::parser::parse_module(source)
                .map_err(|error| refuse(format!("comptime source `{name}` failed to parse: {error}")))?;
            Ok((name, module))
        }).collect::<Result<_, PipelineError>>()?;
        Ok(Self { original_digest, modules, selected: selected.into(), owners, user_modules })
    }

    /// Select an original item, never a statement/body supplied by the caller.
    /// Only the first original comptime block has authenticated empty prior
    /// output. Replaying this immutable recipe creates a fresh runtime session.
    pub fn check_block(&self, item: usize) -> Result<CheckedComptimeRecipe, PipelineError> {
        let (_, module) = self.modules.iter().find(|(name, _)| name == &self.selected)
            .expect("selected raw module retained");
        let first = module.items.iter().position(|item| matches!(item, Item::Comptime(_)))
            .ok_or_else(|| refuse("comptime session has no original block"))?;
        if item != first {
            return Err(refuse("comptime session requires the first original block; later blocks need authenticated prior output"));
        }
        let Item::Comptime(body) = &module.items[item] else { unreachable!("first block selected") };
        let line = module.item_lines.get(item).copied().unwrap_or(0);
        let mut hash = blake3::Hasher::new();
        hash.update(b"witchy.comptime-block-recipe.v1\0");
        hash.update(&REWRITE_VERSION.to_le_bytes());
        hash.update(&self.original_digest);
        hash.update(&(self.selected.len() as u64).to_le_bytes());
        hash.update(self.selected.as_bytes());
        hash.update(&(item as u64).to_le_bytes());
        hash.update(&line.to_le_bytes());
        // The only supported prior-output state is genuinely empty; no caller
        // can provide a digest purporting to authenticate generated ASTs.
        hash.update(&0_u64.to_le_bytes());
        for (name, owner) in self.owners.assignments() {
            super::super::static_evaluation::hash_owner_assignment(&mut hash, name, owner);
        }
        let digest = *hash.finalize().as_bytes();
        let scope = format!("comptime:{}:{}:1", self.selected.len(), self.selected);
        let contract = CompilerSessionContract::for_source(digest, scope).map_err(refuse)?;
        let mut prepared = module.clone();
        let keep = |item: &Item| match item {
            Item::Comptime(_) => false,
            Item::Function(function) if function.name == "main" => false,
            _ => true,
        };
        prepared.items = module.items.iter().filter(|item| keep(item)).cloned().collect();
        prepared.item_lines = module.items.iter().enumerate().filter(|(_, item)| keep(item))
            .map(|(index, _)| module.item_lines.get(index).copied().unwrap_or(0)).collect();
        let mut body = body.clone();
        body.stmts.push(Stmt::Expr(Expr::Int(0)));
        body.lines.push(line);
        prepared.items.push(Item::Function(Function {
            line, public: false, pure: false, comptime_only: false, attributes: Vec::new(),
            name: "main".into(), type_params: Vec::new(), params: Vec::new(),
            ret: Some(Type::Named("Int".into(), Vec::new())), body,
            bounds: Vec::new(), is_gen: false, is_async: false,
        }));
        prepared.item_lines.push(line);
        install_services(&mut prepared);
        // Relocation selects the existing compiler syntax namespace, while the
        // declaration owner remains the exact loader identity of the source.
        let selected_owner = self.owners.owner(&self.selected).expect("owner validated").clone();
        let assignments = self.owners.assignments().filter(|(name, _)| *name != self.selected)
            .map(|(name, owner)| (name.to_owned(), owner.clone()))
            .chain(std::iter::once(("comptime".into(), selected_owner)));
        let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)?;
        let mut user_modules = self.user_modules.clone();
        user_modules.remove(&self.selected);
        user_modules.insert("comptime".into());
        let modules = self.modules.iter().filter(|(name, _)| name != &self.selected).cloned()
            .chain(std::iter::once(("comptime".into(), prepared))).collect();
        let expand = ComptimeExpander::with_sibling_request(require_expansion_free, |_| false);
        let (checked, typed) = link_checked_with(modules, "comptime", expand, LinkMode::Production,
            Some(&user_modules), true, Some(owners))?;
        let typed = typed.ok_or_else(|| refuse("comptime block link did not retain checked source"))?;
        let checked = finish_canonical_source(checked, typed, digest, PrecompiledDeclarationEnvironment::default())?;
        bind_recipe(checked, &contract)
    }
}

// This fixed callback never mutates any module. A public callback here could
// replace the selected body or execute sibling comptime against unrecorded
// prior output. Pending expansion instead stays outside this first-entry lane.
fn require_expansion_free(
    name: &str, module: &mut Module, _: &[(String, Module)],
) -> Result<witchy_syntax::origin::OriginTable, String> {
    let refusal = || format!("comptime session module `{name}` requires authenticated prior expansion");
    if module.items.iter().any(|item| matches!(item, Item::Comptime(_))) {
        return Err(refusal());
    }
    witchy_syntax::ast::visit::visit_module_exprs(module, &mut |expression| {
        if matches!(expression, Expr::TaggedLit { .. })
            || matches!(expression, Expr::Call { name, .. } if name == "@hole_origin")
        { return Err(refusal()); }
        Ok(())
    })?;
    Ok(witchy_syntax::origin::OriginTable::default())
}
