//! Source-selected compiler entry. The first block accepts only raw source;
//! successor checking accepts typed prior-output data, whose execution
//! authenticity must be established by the compiler-owned runtime driver.
use super::*;
use witchy_syntax::ast::{Expr, Function, Stmt};
use witchy_syntax::compiler_session::ComptimeSyntaxOrigin;

const REWRITE_VERSION: u32 = 2;

/// Checked syntax data from one preceding original block. This is not an
/// execution receipt: the compiler-owned caller must compare the runtime's
/// admitted recipe digest before handing its actual output to this checker.
#[derive(Clone)]
pub struct PriorComptimeBlockOutput {
    pub original_item: usize,
    pub items: Vec<(Item, ComptimeSyntaxOrigin)>,
}

/// Immutable original-source session. This is an explicit recipe lane, not the
/// default expansion implementation. Its private fields contain parsed raw
/// bytes and loader assignments; no mutable module accessor is exposed.
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
    pub fn original_block_items(&self) -> Vec<usize> {
        let (_, module) = self.modules.iter().find(|(name, _)| name == &self.selected)
            .expect("selected raw module retained");
        module.items.iter().enumerate().filter_map(|(index, item)|
            matches!(item, Item::Comptime(_)).then_some(index)).collect()
    }

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

    /// Select an original item with genuinely empty prior output. Replaying
    /// this immutable recipe creates a fresh runtime session.
    pub fn check_block(&self, item: usize) -> Result<CheckedComptimeRecipe, PipelineError> {
        let (_, module) = self.modules.iter().find(|(name, _)| name == &self.selected)
            .expect("selected raw module retained");
        let first = module.items.iter().position(|item| matches!(item, Item::Comptime(_)))
            .ok_or_else(|| refuse("comptime session has no original block"))?;
        if item != first {
            return Err(refuse("comptime session requires the first original block; later blocks need authenticated prior output"));
        }
        self.check_block_with_prior_output(item, &[])
    }

    /// Low-level successor checker. Prior items are data, not proof that the
    /// preceding recipe ran; only a compiler-owned orchestration path that
    /// checks the admitted runtime outcome's recipe digest may call this as an
    /// authenticated sequence. The prefix/order and every destination item
    /// are independently checked here.
    pub fn check_block_with_prior_output(
        &self, item: usize, prior: &[PriorComptimeBlockOutput],
    ) -> Result<CheckedComptimeRecipe, PipelineError> {
        let (_, module) = self.modules.iter().find(|(name, _)| name == &self.selected)
            .expect("selected raw module retained");
        let blocks: Vec<_> = module.items.iter().enumerate()
            .filter_map(|(index, item)| matches!(item, Item::Comptime(_)).then_some(index)).collect();
        let Some(ordinal) = blocks.iter().position(|index| *index == item) else {
            return Err(refuse("comptime session requires the first original block or an ordered original successor"));
        };
        if prior.len() != ordinal || prior.iter().zip(&blocks).any(|(output, index)| output.original_item != *index) {
            return Err(refuse("comptime session requires the complete ordered original-block output prefix"));
        }
        #[cfg(target_arch = "wasm32")]
        if !prior.is_empty() { return Err(refuse("ordered comptime source sessions require the native compiler")); }
        if prior.iter().flat_map(|output| &output.items).any(|(item, _)|
            matches!(item, Item::Comptime(_)) || matches!(item, Item::Function(function) if function.name == "main")) {
            return Err(refuse("comptime session cannot use a generated comptime block or displaced main as prior output"));
        }
        self.check_prior_items(prior)?;
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
        #[cfg(not(target_arch = "wasm32"))]
        hash_prior_items(&mut hash, prior)?;
        #[cfg(target_arch = "wasm32")]
        hash.update(&0_u64.to_le_bytes());
        for (name, owner) in self.owners.assignments() {
            super::super::static_evaluation::hash_owner_assignment(&mut hash, name, owner);
        }
        let digest = *hash.finalize().as_bytes();
        let scope = format!("comptime:{}:{}:{}", self.selected.len(), self.selected, ordinal + 1);
        let contract = CompilerSessionContract::for_source(digest, scope).map_err(refuse)?;
        let mut prepared = materialize_prior(module, prior, false);
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
        // Earlier output was checked above. Only behavior called by this block
        // belongs in its zero-grant executable recipe; unrelated emitted
        // functions may require runtime capabilities.
        retain_referenced_functions(&mut prepared, &HashSet::new(), true);
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

    /// Check prior admitted items as source data before running a successor.
    /// The source main and unrelated functions can refer to later output, so
    /// they do not belong in this prefix check. Final destination checking
    /// still checks the complete module after all blocks execute.
    fn check_prior_items(&self, prior: &[PriorComptimeBlockOutput]) -> Result<(), PipelineError> {
        if prior.is_empty() { return Ok(()); }
        let (_, original) = self.modules.iter().find(|(name, _)| name == &self.selected)
            .expect("selected raw module retained");
        let mut prepared = materialize_prior(original, prior, false);
        let emitted_functions = prior.iter().flat_map(|output| &output.items)
            .filter_map(|(item, _)| match item { Item::Function(function) => Some(function.name.clone()), _ => None })
            .collect();
        retain_referenced_functions(&mut prepared, &emitted_functions, false);
        let modules = self.modules.iter().map(|(name, module)| (
            name.clone(), if name == &self.selected { prepared.clone() } else { module.clone() },
        )).collect();
        let expand = ComptimeExpander::with_sibling_request(require_expansion_free, |_| false);
        let _ = link_checked_with(modules, &self.selected, expand, LinkMode::Production,
            Some(&self.user_modules), true, Some(self.owners.clone()))?;
        Ok(())
    }

    /// Check the ordinary destination after all original blocks have run.
    /// The caller must have verified every predecessor's admitted outcome;
    /// this method independently rechecks the generated items in context.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn check_expanded_with_prior_output(
        &self, prior: &[PriorComptimeBlockOutput],
    ) -> Result<CheckedModule, PipelineError> {
        self.check_expanded_with_display_module(prior).map(|(checked, _)| checked)
    }

    /// Retain the source-shaped selected module for compiler tooling after
    /// the same destination check used by ordinary compilation succeeds.
    /// Prior output remains data until the compiler-owned caller authenticates
    /// every admitted predecessor before reaching this method.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn check_expanded_with_display_module(
        &self, prior: &[PriorComptimeBlockOutput],
    ) -> Result<(CheckedModule, Module), PipelineError> {
        let blocks = self.original_block_items();
        if blocks.len() != prior.len() || prior.iter().zip(&blocks)
            .any(|(output, index)| output.original_item != *index) {
            return Err(refuse("comptime destination requires every original block output in order"));
        }
        if prior.iter().flat_map(|output| &output.items).any(|(item, _)| matches!(item, Item::Comptime(_))) {
            return Err(refuse("generated comptime blocks need a separate authenticated execution step"));
        }
        let (_, original) = self.modules.iter().find(|(name, _)| name == &self.selected)
            .expect("selected raw module retained");
        let expanded = materialize_prior(original, prior, true);
        let modules = self.modules.iter().map(|(name, module)| (
            name.clone(), if name == &self.selected { expanded.clone() } else { module.clone() },
        )).collect();
        let mut hash = blake3::Hasher::new();
        hash.update(b"witchy.comptime-expanded-source.v1\0");
        hash.update(&REWRITE_VERSION.to_le_bytes());
        hash.update(&self.original_digest);
        hash.update(&(self.selected.len() as u64).to_le_bytes());
        hash.update(self.selected.as_bytes());
        hash_prior_items(&mut hash, prior)?;
        for (name, owner) in self.owners.assignments() {
            super::super::static_evaluation::hash_owner_assignment(&mut hash, name, owner);
        }
        let digest = *hash.finalize().as_bytes();
        let expand = ComptimeExpander::with_sibling_request(require_expansion_free, |_| false);
        let (checked, typed) = link_checked_with(modules, &self.selected, expand, LinkMode::Production,
            Some(&self.user_modules), true, Some(self.owners.clone()))?;
        let typed = typed.ok_or_else(|| refuse("comptime destination did not retain checked source"))?;
        let checked = finish_canonical_source(checked, typed, digest, PrecompiledDeclarationEnvironment::default())?;
        Ok((checked, expanded))
    }
}

/// A compiler recipe executes only its selected block and helpers reachable
/// from that block. Runtime functions elsewhere in the source can require
/// capabilities unavailable to this zero-grant session. The Debug rendering
/// over-approximates names across calls, function values and generated syntax;
/// an omitted real reference still fails closed during ordinary checking.
fn retain_referenced_functions(module: &mut Module, prior_functions: &HashSet<String>, keep_entry: bool) {
    let entry = module.items.len() - 1;
    let mut keep: Vec<bool> = module.items.iter().enumerate().map(|(index, item)|
        (keep_entry && index == entry) || !matches!(item, Item::Function(_))
            || matches!(item, Item::Function(function) if prior_functions.contains(&function.name))).collect();
    let mut referenced = String::new();
    for (index, item) in module.items.iter().enumerate() {
        if keep[index] { referenced.push_str(&format!("{item:?}")); }
    }
    loop {
        let mut added = false;
        for (index, item) in module.items.iter().enumerate() {
            if keep[index] { continue; }
            let Item::Function(function) = item else { continue; };
            if referenced.contains(&format!("{:?}", function.name)) {
                keep[index] = true;
                referenced.push_str(&format!("{item:?}"));
                added = true;
            }
        }
        if !added { break; }
    }
    let mut index = 0;
    module.items.retain(|_| { let retained = keep[index]; index += 1; retained });
    let mut index = 0;
    module.item_lines.retain(|_| { let retained = keep[index]; index += 1; retained });
}

fn materialize_prior(module: &Module, prior: &[PriorComptimeBlockOutput], keep_main: bool) -> Module {
    let mut prepared = module.clone();
    prepared.items.clear();
    prepared.item_lines.clear();
    let mut prior_cursor = 0;
    for (index, original) in module.items.iter().enumerate() {
        if matches!(original, Item::Comptime(_)) {
            if prior_cursor < prior.len() && prior[prior_cursor].original_item == index {
                for (generated, origin) in &prior[prior_cursor].items {
                    prepared.items.push(generated.clone());
                    prepared.item_lines.push(origin.definition_line);
                }
                prior_cursor += 1;
            }
        } else if keep_main || !matches!(original, Item::Function(function) if function.name == "main") {
            prepared.items.push(original.clone());
            prepared.item_lines.push(module.item_lines.get(index).copied().unwrap_or(0));
        }
    }
    prepared
}

#[cfg(not(target_arch = "wasm32"))]
fn hash_prior_items(hash: &mut blake3::Hasher, prior: &[PriorComptimeBlockOutput]) -> Result<(), PipelineError> {
    hash.update(&(prior.len() as u64).to_le_bytes());
    for output in prior {
        hash.update(&(output.original_item as u64).to_le_bytes());
        hash.update(&(output.items.len() as u64).to_le_bytes());
        for (item, origin) in &output.items {
            let bytes = postcard::to_allocvec(&(item, origin))
                .map_err(|error| refuse(format!("comptime prior item could not be encoded: {error}")))?;
            hash.update(&(bytes.len() as u64).to_le_bytes());
            hash.update(&bytes);
        }
    }
    Ok(())
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
