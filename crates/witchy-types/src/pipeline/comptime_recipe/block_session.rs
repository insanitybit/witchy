//! Source-selected compiler entry. Each block accepts the complete ordered
//! prior-output chain, whose execution authenticity is established by the
//! compiler-owned runtime driver.
use super::*;
use std::sync::{Arc, Mutex};
use witchy_syntax::ast::{Expr, Function, Stmt};
use witchy_syntax::compiler_session::ComptimeSyntaxOrigin;

const REWRITE_VERSION: u32 = 6;
const MAX_CACHED_PREFIX_VALIDATIONS: usize = 256;

#[derive(Hash, PartialEq, Eq)]
struct PrefixValidationKey {
    source_context: [u8; 32],
    selected: String,
    prior_output: [u8; 32],
}

fn type_operand(expression: &Expr) -> Result<Type, PipelineError> {
    match expression {
        Expr::Ctor { name, args } => Ok(Type::Named(
            name.clone(),
            args.iter().map(type_operand).collect::<Result<_, _>>()?,
        )),
        Expr::Call { name, args } if name.split_once('.').is_some_and(|(_, type_name)|
            type_name.chars().next().is_some_and(char::is_uppercase)) =>
            Ok(Type::Named(name.clone(), args.iter().map(type_operand).collect::<Result<_, _>>()?)),
        Expr::Field { base, field } if matches!(&**base, Expr::Var(_)) => {
            let Expr::Var(module) = &**base else {
                unreachable!()
            };
            Ok(Type::Named(format!("{module}.{field}"), Vec::new()))
        }
        _ => Err(refuse(
            "type_info operand must be a type name, not a runtime expression",
        )),
    }
}

fn resolve_type_info_calls(
    body: &mut witchy_syntax::ast::Block,
    stage: &[(String, Module)], selected: &str, user_modules: &HashSet<String>,
) -> Result<bool, PipelineError> {
    let mut used = false;
    witchy_syntax::ast::visit::visit_block_mut(body, &mut |expression| {
        if let Expr::Call { name, args } = expression {
            if name == "type_info" {
                if args.len() != 1 {
                    return Err(refuse("type_info expects exactly one type operand"));
                }
                let requested = type_operand(&args[0])?;
                *expression = witchy_syntax::reflect::type_info_expr_for_type_in_modules(
                    stage, selected, user_modules, requested,
                ).map_err(refuse)?;
                used = true;
            }
        }
        Ok(())
    })?;
    Ok(used)
}

fn resolve_stage_type_info(
    prepared: &mut Module, modules: &[(String, Module)],
    selected: &str, user_modules: &HashSet<String>,
) -> Result<(), PipelineError> {
    let mut uses_type_info = false;
    for item in &mut prepared.items {
        if let Item::Function(function) = item {
            // `main` is the compiler's synthetic block body in a recipe.
            // Ordinary runtime functions cannot acquire a type lookup merely
            // because a compile-time block happens to call them.
            if !function.comptime_only && function.name != "main" { continue; }
            uses_type_info |= resolve_type_info_calls(
                &mut function.body, modules, selected, user_modules,
            )?;
        }
    }
    if uses_type_info && !prepared.imports.iter().any(|name| name == "meta") {
        prepared.imports.push("meta".into());
    }
    Ok(())
}

fn local_tag_recipe_digest(
    session: &ComptimeSourceSession,
    prior: &[PriorComptimeBlockOutput],
    prior_tags: &[PriorLocalTagOutput],
    tag: &str,
    part: &str,
    line: u32,
) -> Result<[u8; 32], PipelineError> {
    let mut hash = blake3::Hasher::new();
    hash.update(b"witchy.comptime-local-tag-recipe.v1\0");
    hash.update(&REWRITE_VERSION.to_le_bytes());
    hash.update(&session.original_digest);
    hash.update(&(session.selected.len() as u64).to_le_bytes());
    hash.update(session.selected.as_bytes());
    hash.update(&line.to_le_bytes());
    hash.update(&(tag.len() as u64).to_le_bytes());
    hash.update(tag.as_bytes());
    hash.update(&(part.len() as u64).to_le_bytes());
    hash.update(part.as_bytes());
    #[cfg(not(target_arch = "wasm32"))]
    hash_prior_items(&mut hash, prior)?;
    #[cfg(not(target_arch = "wasm32"))]
    for output in prior_tags {
        hash.update(&output.recipe_digest);
        let encoded = postcard::to_allocvec(&(&output.expression, &output.origin))
            .map_err(|error| refuse(format!("cannot bind prior tag output: {error}")))?;
        hash.update(&(encoded.len() as u64).to_le_bytes());
        hash.update(&encoded);
    }
    for (name, owner) in session.owners.assignments() {
        super::super::static_evaluation::hash_owner_assignment(&mut hash, name, owner);
    }
    Ok(*hash.finalize().as_bytes())
}

fn next_local_tag_identity(
    session: &ComptimeSourceSession,
    prior: &[PriorComptimeBlockOutput],
    prior_tags: &[PriorLocalTagOutput],
) -> Result<Option<(String, String, u32, [u8; 32])>, PipelineError> {
    let (_, original) = session.modules.iter().find(|(name, _)| name == &session.selected)
        .expect("selected raw module retained");
    let display = materialize_prior(original, prior, true, false);
    let mut selected = None;
    let mut seen = 0;
    witchy_syntax::ast::visit::visit_module_exprs(&display, &mut |expression| {
        if let Expr::TaggedLit { tag, parts, holes, line, .. } = expression {
            if seen == prior_tags.len() {
                selected = Some((tag.clone(), parts.clone(), holes.len(), *line));
            }
            seen += 1;
        }
        Ok::<_, ()>(())
    }).expect("tag selection cannot fail");
    if seen < prior_tags.len() {
        return Err(refuse("prior tag output exceeds source invocations"));
    }
    let Some((tag, parts, holes, line)) = selected else { return Ok(None); };
    if holes != 0 || parts.len() != 1 {
        return Err(refuse("admitted local tag recipe requires one static part and no holes"));
    }
    let digest = local_tag_recipe_digest(session, prior, prior_tags, &tag, &parts[0], line)?;
    Ok(Some((tag, parts[0].clone(), line, digest)))
}

/// One expression returned by a compiler-authenticated local tag recipe.
#[derive(Clone)]
pub struct PriorLocalTagOutput {
    pub recipe_digest: [u8; 32],
    pub expression: Expr,
    pub origin: ComptimeSyntaxOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComptimeBlockSite {
    Original(usize),
    Generated { parent_output: usize, emitted_item: usize },
}

/// Checked syntax data from one preceding source or generated block. This is not an
/// execution receipt: the compiler-owned caller must compare the runtime's
/// admitted recipe digest before handing its actual output to this checker.
#[derive(Clone)]
pub struct PriorComptimeBlockOutput {
    pub site: ComptimeBlockSite,
    pub items: Vec<(Item, ComptimeSyntaxOrigin)>,
}

/// Immutable source session. This is an explicit recipe lane, not the
/// default expansion implementation. Its private fields contain parsed raw
/// bytes and loader assignments; no mutable module accessor is exposed.
pub struct ComptimeSourceSession {
    original_digest: [u8; 32],
    module_context_digest: [u8; 32],
    raw_sources: Arc<Vec<(String, Vec<u8>)>>,
    modules: Arc<Vec<(String, Module)>>,
    selected: String,
    owners: AuthenticatedModuleOwners,
    user_modules: HashSet<String>,
    validated_prefixes: Arc<Mutex<std::collections::HashSet<PrefixValidationKey>>>,
}

fn refuse(message: impl Into<String>) -> PipelineError {
    PipelineError::Type(TypeError { message: message.into() })
}

impl ComptimeSourceSession {
    pub fn producer_dependency_names(&self, producers: &[String]) -> Vec<String> {
        let dependencies = self.producer_dependencies();
        producers.iter().filter(|name| *name != &self.selected && dependencies.contains(*name))
            .cloned().collect()
    }

    /// Select another loader-owned module from this already authenticated,
    /// immutable bundle without reparsing the raw source modules.
    pub fn select_module(&self, selected: &str) -> Result<Self, PipelineError> {
        if self.raw_sources.iter().any(|(name, _)| name.is_empty())
            || self.raw_sources.windows(2).any(|pair| pair[0].0 == pair[1].0)
            || !self.raw_sources.iter().any(|(name, _)| name == selected)
            || (selected != "comptime" && self.raw_sources.iter().any(|(name, _)| name == "comptime"))
        {
            return Err(refuse("comptime session requires unique raw sources and an unambiguous selected module"));
        }
        self.owners.validate_module_names(self.raw_sources.iter().map(|(name, _)| name.clone()))?;
        if !self.user_modules.contains(selected) {
            return Err(refuse("comptime session selected module is not a loader-owned user module"));
        }
        Ok(Self {
            original_digest: checked_source_digest(
                &self.raw_sources, selected, &PrecompiledDeclarationEnvironment::default(),
            ),
            module_context_digest: self.module_context_digest,
            raw_sources: Arc::clone(&self.raw_sources),
            modules: Arc::clone(&self.modules),
            selected: selected.into(),
            owners: self.owners.clone(),
            user_modules: self.user_modules.clone(),
            validated_prefixes: Arc::clone(&self.validated_prefixes),
        })
    }

    /// Build a producer's stage from admitted predecessors. The caller owns
    /// runtime authentication; every stage rechecks complete output prefixes
    /// and binds their syntax to the successor recipe digest.
    pub fn from_source_bundle_with_predecessors(
        sources: Vec<(String, Vec<u8>)>, selected: &str,
        user_modules: HashSet<String>, owners: AuthenticatedModuleOwners,
        predecessors: &[(String, Vec<PriorComptimeBlockOutput>)],
    ) -> Result<Self, PipelineError> {
        Self::from_source_bundle(sources, selected, user_modules, owners)?
            .with_predecessors(predecessors)
    }

    /// Apply authenticated predecessor outputs to a previously selected
    /// module. This keeps graph traversal on the same parsed source bundle.
    pub fn with_predecessors(
        self,
        predecessors: &[(String, Vec<PriorComptimeBlockOutput>)],
    ) -> Result<Self, PipelineError> {
        let mut session = self;
        let selected = session.selected.clone();
        let producers = session.modules.iter().filter(|(name, module)|
            session.user_modules.contains(name) && module.items.iter().any(|item| matches!(item, Item::Comptime(_))))
            .map(|(name, _)| name.clone()).collect::<Vec<_>>();
        let expected = session.producer_dependency_names(&producers);
        if expected.len() != predecessors.len() || expected.iter().any(|name|
            !predecessors.iter().any(|(predecessor, _)| predecessor == name)) {
            return Err(refuse("comptime producer requires all admitted dependency outputs"));
        }
        let mut seen = HashSet::new();
        let mut hash = blake3::Hasher::new();
        hash.update(b"witchy.comptime-predecessor-stage.v1\0");
        hash.update(&REWRITE_VERSION.to_le_bytes());
        hash.update(&session.original_digest);
        let mut context_hash = blake3::Hasher::new();
        context_hash.update(b"witchy.comptime-source-context.v1\0");
        context_hash.update(&session.module_context_digest);
        for (predecessor, prior) in predecessors {
            if !seen.insert(predecessor.clone()) || predecessor == &selected {
                return Err(refuse("comptime predecessor outputs must be unique and acyclic"));
            }
            let stage = session.select_module(predecessor)?;
            let dependencies = stage.producer_dependency_names(&producers);
            if dependencies.iter().any(|name| !seen.contains(name)) {
                return Err(refuse("comptime predecessor outputs are out of dependency order"));
            }
            stage.check_completed_prefix(prior)?;
            if prior.iter().flat_map(|output| &output.items).any(|(item, _)|
                matches!(item, Item::Function(function) if function.name == "main")) {
                return Err(refuse("comptime predecessor cannot displace a source main"));
            }
            let (_, original) = stage.modules.iter().find(|(name, _)| name == predecessor)
                .expect("predecessor raw module retained");
            let expanded = materialize_prior(original, prior, true, false);
            Arc::make_mut(&mut session.modules).iter_mut().find(|(name, _)| name == predecessor)
                .expect("predecessor raw module retained").1 = expanded;
            hash.update(&(predecessor.len() as u64).to_le_bytes());
            hash.update(predecessor.as_bytes());
            hash_prior_items(&mut hash, prior)?;
            context_hash.update(&(predecessor.len() as u64).to_le_bytes());
            context_hash.update(predecessor.as_bytes());
            hash_prior_items(&mut context_hash, prior)?;
            session.module_context_digest = *context_hash.clone().finalize().as_bytes();
        }
        session.original_digest = *hash.finalize().as_bytes();
        Ok(session)
    }

    pub fn check_completed_prefix(&self, prior: &[PriorComptimeBlockOutput]) -> Result<(), PipelineError> {
        if self.next_block_site(prior)?.is_some() {
            return Err(refuse("comptime producer has unexecuted blocks"));
        }
        self.check_prior_items(prior)
    }

    /// Select a local hole-free tag from the authenticated source and execute
    /// its body with the same zero-grant recipe boundary as an original block.
    /// The returned expression is still data; destination replacement is a
    /// separate checked step.
    pub fn check_first_local_tag_recipe(
        &self, prior: &[PriorComptimeBlockOutput],
    ) -> Result<CheckedComptimeRecipe, PipelineError> {
        self.check_next_local_tag_recipe(prior, &[])?.ok_or_else(|| refuse("source session has no tagged literal"))
    }

    pub fn check_next_local_tag_recipe(
        &self, prior: &[PriorComptimeBlockOutput], prior_tags: &[PriorLocalTagOutput],
    ) -> Result<Option<CheckedComptimeRecipe>, PipelineError> {
        self.check_completed_prefix(prior)?;
        let (_, original) = self.modules.iter().find(|(name, _)| name == &self.selected)
            .expect("selected raw module retained");
        let Some((tag, part, line, digest)) = next_local_tag_identity(self, prior, prior_tags)? else { return Ok(None); };
        let mut prepared = materialize_prior(original, prior, true, true);
        let function = prepared.items.iter().find_map(|item| match item {
            Item::Function(function) if function.name == tag => Some(function),
            _ => None,
        }).ok_or_else(|| refuse("admitted local tag recipe requires a source-defined local tag"))?;
        if !function.comptime_only || function.params.len() != 2
            || !matches!(function.ret.as_ref(), Some(Type::Named(name, args))
                if (name == "meta.ExprSyntax" || name == "ExprSyntax") && args.is_empty())
        {
            return Err(refuse("admitted local tag must be a comptime function returning meta.ExprSyntax with two parameters"));
        }
        let scope = format!("comptime:tag:{}:{}", self.selected, line);
        let contract = CompilerSessionContract::for_source(digest, scope).map_err(refuse)?;
        if let Some(index) = prepared.items.iter().position(|item|
            matches!(item, Item::Function(function) if function.name == "main")) {
            prepared.items.remove(index);
            prepared.item_lines.remove(index);
        }
        let call = Expr::Call { name: tag, args: vec![Expr::List(vec![Expr::Str(part)]), Expr::List(Vec::new())] };
        let body = witchy_syntax::ast::Block {
            stmts: vec![Stmt::Expr(Expr::Call { name: "emit_expr".into(), args: vec![call] }), Stmt::Expr(Expr::Int(0))],
            lines: vec![line, line], region: None,
        };
        prepared.items.push(Item::Function(Function {
            line, public: false, pure: false, comptime_only: false, attributes: Vec::new(),
            name: "main".into(), type_params: Vec::new(), params: Vec::new(),
            ret: Some(Type::Named("Int".into(), Vec::new())), body,
            bounds: Vec::new(), is_gen: false, is_async: false,
        }));
        prepared.item_lines.push(line);
        retain_referenced_functions(&mut prepared, &HashSet::new(), true);
        let type_stage = self.modules.iter().map(|(name, module)| (
            name.clone(), if name == &self.selected { prepared.clone() } else { module.clone() },
        )).collect::<Vec<_>>();
        resolve_stage_type_info(&mut prepared, &type_stage, &self.selected, &self.user_modules)?;
        install_services(&mut prepared);
        let selected_owner = self.owners.owner(&self.selected).expect("owner validated").clone();
        let assignments = self.owners.assignments().filter(|(name, _)| *name != self.selected)
            .map(|(name, owner)| (name.to_owned(), owner.clone()))
            .chain(std::iter::once(("comptime".into(), selected_owner)));
        let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)?;
        let dependencies = self.producer_dependencies();
        let mut user_modules = self.user_modules.iter()
            .filter(|name| dependencies.contains(*name)).cloned().collect::<HashSet<_>>();
        user_modules.remove(&self.selected);
        user_modules.insert("comptime".into());
        let modules = self.modules.iter().filter(|(name, _)| name != &self.selected && dependencies.contains(name)).cloned()
            .chain(std::iter::once(("comptime".into(), prepared))).collect();
        let expand = ComptimeExpander::with_sibling_request(require_expansion_free, |_| false);
        let (checked, typed) = link_checked_with(modules, "comptime", expand, LinkMode::Production,
            Some(&user_modules), true, Some(owners))?;
        let typed = typed.ok_or_else(|| refuse("admitted tag recipe did not retain checked source"))?;
        let checked = finish_canonical_source(checked, typed, digest, PrecompiledDeclarationEnvironment::default())?;
        bind_recipe(checked, &contract).map(Some)
    }

    /// Recheck one actual admitted tag result in the ordinary destination.
    /// The compiler driver compares the runtime receipt with the sealed
    /// recipe's digest before passing its expression here; this method binds
    /// that recipe back to the immutable source and exact prior outputs.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn check_expanded_first_local_tag_output(
        &self, prior: &[PriorComptimeBlockOutput], recipe: &CheckedComptimeRecipe,
        expression: Expr, origin: ComptimeSyntaxOrigin,
    ) -> Result<CheckedModule, PipelineError> {
        self.check_expanded_local_tag_outputs(prior, &[PriorLocalTagOutput {
            recipe_digest: recipe.session_source_digest(), expression, origin,
        }])
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn check_expanded_local_tag_outputs(
        &self, prior: &[PriorComptimeBlockOutput], prior_tags: &[PriorLocalTagOutput],
    ) -> Result<CheckedModule, PipelineError> {
        if prior_tags.is_empty() {
            return Err(refuse("admitted tag destination has no outputs"));
        }
        for output in prior_tags {
            witchy_syntax::ast::visit::visit_expr(&output.expression, &mut |expression| {
                if matches!(expression, Expr::TaggedLit { .. }) {
                    return Err(refuse("admitted local tag output cannot introduce another tag"));
                }
                Ok(())
            })?;
        }
        for index in 0..prior_tags.len() {
            let Some((_, _, _, expected_digest)) = next_local_tag_identity(self, prior, &prior_tags[..index])? else {
                return Err(refuse("admitted tag destination has too many outputs"));
            };
            if prior_tags[index].recipe_digest != expected_digest {
                return Err(refuse("admitted tag result belongs to a different source recipe"));
            }
        }
        if next_local_tag_identity(self, prior, prior_tags)?.is_some() {
            return Err(refuse("admitted tag destination has unexpanded invocations"));
        }
        let recipe_digest = prior_tags.last().expect("nonempty").recipe_digest;
        let (_, original) = self.modules.iter().find(|(name, _)| name == &self.selected)
            .expect("selected raw module retained");
        let (mut expanded, emitted_origins) = materialize_prior_with_origins(original, prior, true, false);
        let mut invocations = Vec::new();
        for output in prior_tags {
            let (tag, invocation_line) = replace_first_tagged_literal(&mut expanded, output.expression.clone())
                .ok_or_else(|| refuse("admitted tag destination has no selected invocation"))?;
            invocations.push((tag, invocation_line));
        }
        let modules = self.modules.iter().map(|(name, module)| (
            name.clone(), if name == &self.selected { expanded.clone() } else { module.clone() },
        )).collect::<Vec<_>>();
        let mut hash = blake3::Hasher::new();
        hash.update(b"witchy.comptime-local-tag-destination.v2\0");
        hash.update(&REWRITE_VERSION.to_le_bytes());
        hash.update(&recipe_digest);
        hash_prior_items(&mut hash, prior)?;
        let encoded = postcard::to_allocvec(&expanded)
            .map_err(|error| refuse(format!("cannot bind admitted tag syntax: {error}")))?;
        hash.update(&(encoded.len() as u64).to_le_bytes());
        hash.update(&encoded);
        for output in prior_tags {
            let encoded = postcard::to_allocvec(&output.origin)
                .map_err(|error| refuse(format!("cannot bind admitted tag origin: {error}")))?;
            hash.update(&(encoded.len() as u64).to_le_bytes());
            hash.update(&encoded);
        }
        for (name, owner) in self.owners.assignments() {
            super::super::static_evaluation::hash_owner_assignment(&mut hash, name, owner);
        }
        let digest = *hash.finalize().as_bytes();
        let expand = ComptimeExpander::with_sibling_request(require_expansion_free, |_| false);
        let (mut checked, typed) = link_checked_with(modules, &self.selected, expand,
            LinkMode::Production, Some(&self.user_modules), true, Some(self.owners.clone()))?;
        retain_emitted_origins(&mut checked, &self.selected, &emitted_origins);
        for (tag, invocation_line) in &invocations {
            let definition_line = original.items.iter().enumerate().find_map(|(index, item)|
                matches!(item, Item::Function(function) if function.name == *tag)
                    .then(|| original.item_lines.get(index).copied().unwrap_or(0)))
                .unwrap_or(0);
            checked.linked.origins.record_tagged_literal(
                &self.selected, format!("{}.{}", self.selected, tag), tag.clone(),
                witchy_syntax::origin::SourceSpan::line(&self.selected, definition_line),
                witchy_syntax::origin::SourceSpan::line(&self.selected, *invocation_line), Vec::new(),
            );
        }
        let typed = typed.ok_or_else(|| refuse("admitted tag destination did not retain checked source"))?;
        finish_canonical_source(checked, typed, digest, PrecompiledDeclarationEnvironment::default())
    }

    fn producer_dependencies(&self) -> HashSet<String> {
        let mut reachable = HashSet::from([self.selected.clone()]);
        loop {
            let mut changed = false;
            for (name, module) in self.modules.iter() {
                if reachable.contains(name) {
                    for import in &module.imports {
                        changed |= reachable.insert(import.clone());
                    }
                }
            }
            if !changed { return reachable; }
        }
    }

    pub fn original_block_items(&self) -> Vec<usize> {
        let (_, module) = self.modules.iter().find(|(name, _)| name == &self.selected)
            .expect("selected raw module retained");
        module.items.iter().enumerate().filter_map(|(index, item)|
            matches!(item, Item::Comptime(_)).then_some(index)).collect()
    }

    /// Validate the entire admitted prefix and select exactly the next block
    /// in source order, including blocks emitted by an earlier block.
    pub fn next_block_site(
        &self, prior: &[PriorComptimeBlockOutput],
    ) -> Result<Option<ComptimeBlockSite>, PipelineError> {
        fn visit(
            site: ComptimeBlockSite, prior: &[PriorComptimeBlockOutput], cursor: &mut usize,
        ) -> Result<Option<ComptimeBlockSite>, PipelineError> {
            let Some(output) = prior.get(*cursor) else { return Ok(Some(site)); };
            if output.site != site {
                return Err(refuse("comptime session requires the complete ordered block output prefix"));
            }
            let parent_output = *cursor;
            *cursor += 1;
            for (emitted_item, (item, _)) in output.items.iter().enumerate() {
                if matches!(item, Item::Comptime(_)) {
                    let generated = ComptimeBlockSite::Generated { parent_output, emitted_item };
                    if let Some(next) = visit(generated, prior, cursor)? { return Ok(Some(next)); }
                }
            }
            Ok(None)
        }
        let mut cursor = 0;
        for original_item in self.original_block_items() {
            if let Some(next) = visit(ComptimeBlockSite::Original(original_item), prior, &mut cursor)? {
                if cursor != prior.len() {
                    return Err(refuse("comptime session has output after an unexecuted predecessor"));
                }
                return Ok(Some(next));
            }
        }
        if cursor != prior.len() {
            return Err(refuse("comptime session has output beyond its source blocks"));
        }
        Ok(None)
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
        let module_context_digest = source_bundle_context_digest(&sources);
        let modules = sources.iter().map(|(name, bytes)| {
            let source = std::str::from_utf8(&bytes).map_err(|error| refuse(format!("comptime source `{name}` is not UTF-8: {error}")))?;
            let mut module = witchy_syntax::parser::parse_module(source)
                .map_err(|error| refuse(format!("comptime source `{name}` failed to parse: {error}")))?;
            if name == selected && (witchy_syntax::derive::has_only_direct_record_derives(&module)
                || witchy_syntax::derive::has_only_public_state_derives(&module)
                || witchy_syntax::derive::has_only_typed_user_derives(&module)) {
                witchy_syntax::derive::expand_admitted(&mut module)
                    .map_err(|error| refuse(format!("comptime source `{name}` failed to prepare derives: {error}")))?;
            }
            Ok((name.clone(), module))
        }).collect::<Result<_, PipelineError>>()?;
        Ok(Self {
            original_digest,
            module_context_digest,
            raw_sources: Arc::new(sources),
            modules: Arc::new(modules),
            selected: selected.into(),
            owners,
            user_modules,
            validated_prefixes: Arc::new(Mutex::new(std::collections::HashSet::new())),
        })
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
        self.check_site_with_prior_output(ComptimeBlockSite::Original(item), prior)
    }

    pub fn check_next_block_with_prior_output(
        &self, prior: &[PriorComptimeBlockOutput],
    ) -> Result<CheckedComptimeRecipe, PipelineError> {
        let site = self.next_block_site(prior)?
            .ok_or_else(|| refuse("comptime session has no remaining block"))?;
        self.check_site_with_prior_output(site, prior)
    }

    fn check_site_with_prior_output(
        &self, site: ComptimeBlockSite, prior: &[PriorComptimeBlockOutput],
    ) -> Result<CheckedComptimeRecipe, PipelineError> {
        let (_, module) = self.modules.iter().find(|(name, _)| name == &self.selected)
            .expect("selected raw module retained");
        if self.next_block_site(prior)? != Some(site) {
            return Err(refuse("comptime session requires the next authenticated block in source order"));
        }
        #[cfg(target_arch = "wasm32")]
        if !prior.is_empty() { return Err(refuse("ordered comptime source sessions require the native compiler")); }
        if prior.iter().flat_map(|output| &output.items).any(|(item, _)|
            matches!(item, Item::Function(function) if function.name == "main")) {
            return Err(refuse("comptime session cannot use a displaced main as prior output"));
        }
        self.check_prior_items(prior)?;
        let (block, line) = match site {
            ComptimeBlockSite::Original(item) => (&module.items[item],
                module.item_lines.get(item).copied().unwrap_or(0)),
            ComptimeBlockSite::Generated { parent_output, emitted_item } => {
                let (item, origin) = &prior[parent_output].items[emitted_item];
                (item, origin.definition_line)
            }
        };
        let Item::Comptime(body) = block else { unreachable!("next site is a block") };
        let mut hash = blake3::Hasher::new();
        hash.update(b"witchy.comptime-block-recipe.v1\0");
        hash.update(&REWRITE_VERSION.to_le_bytes());
        hash.update(&self.original_digest);
        hash.update(&(self.selected.len() as u64).to_le_bytes());
        hash.update(self.selected.as_bytes());
        hash_block_site(&mut hash, site);
        hash.update(&line.to_le_bytes());
        #[cfg(not(target_arch = "wasm32"))]
        hash_prior_items(&mut hash, prior)?;
        #[cfg(target_arch = "wasm32")]
        hash.update(&0_u64.to_le_bytes());
        for (name, owner) in self.owners.assignments() {
            super::super::static_evaluation::hash_owner_assignment(&mut hash, name, owner);
        }
        let digest = *hash.finalize().as_bytes();
        let scope = format!("comptime:{}:{}:{}", self.selected.len(), self.selected, prior.len() + 1);
        let contract = CompilerSessionContract::for_source(digest, scope).map_err(refuse)?;
        let mut prepared = materialize_prior(module, prior, false, true);
        let mut body = body.clone();
        let mut uses_module_types = false;
        witchy_syntax::ast::visit::visit_block(&body, &mut |expression| {
            if matches!(expression, Expr::Var(name) if name == "module_types") {
                uses_module_types = true;
            }
            Ok::<_, ()>(())
        }).expect("module_types scan cannot fail");
        if uses_module_types {
            // Earlier admitted output is present in `prepared`; later output
            // cannot enter this stage's reflected declaration list.
            let visible_types = witchy_syntax::reflect::module_type_info_exprs(&prepared)
                .map_err(refuse)?;
            let value = if visible_types.is_empty() {
                if !prepared.imports.iter().any(|name| name == "list") {
                    prepared.imports.push("list".into());
                }
                Expr::Call { name: "list.with_capacity".into(), args: vec![Expr::Int(0)] }
            } else { Expr::List(visible_types) };
            body.stmts.insert(0, Stmt::Let {
                ty: Some(Type::Named("List".into(), vec![Type::Named("meta.TypeInfo".into(), Vec::new())])),
                name: "module_types".into(), mutable: false, value,
            });
            body.lines.insert(0, line);
        }
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
        let type_stage = self.modules.iter().map(|(name, module)| (
            name.clone(), if name == &self.selected { prepared.clone() } else { module.clone() },
        )).collect::<Vec<_>>();
        resolve_stage_type_info(&mut prepared, &type_stage, &self.selected, &self.user_modules)?;
        install_services(&mut prepared);
        // Relocation selects the existing compiler syntax namespace, while the
        // declaration owner remains the exact loader identity of the source.
        let selected_owner = self.owners.owner(&self.selected).expect("owner validated").clone();
        let assignments = self.owners.assignments().filter(|(name, _)| *name != self.selected)
            .map(|(name, owner)| (name.to_owned(), owner.clone()))
            .chain(std::iter::once(("comptime".into(), selected_owner)));
        let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)?;
        let dependencies = self.producer_dependencies();
        let mut user_modules = self.user_modules.iter()
            .filter(|name| dependencies.contains(*name)).cloned().collect::<HashSet<_>>();
        user_modules.remove(&self.selected);
        user_modules.insert("comptime".into());
        let modules = self.modules.iter().filter(|(name, _)| name != &self.selected && dependencies.contains(name)).cloned()
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
        let key = prefix_validation_key(self.module_context_digest, &self.selected, prior)?;
        if self.validated_prefixes.lock().ok().is_some_and(|cache| cache.contains(&key)) {
            return Ok(());
        }
        let (_, original) = self.modules.iter().find(|(name, _)| name == &self.selected)
            .expect("selected raw module retained");
        let mut prepared = materialize_prior(original, prior, false, true);
        let emitted_functions = prior.iter().flat_map(|output| &output.items)
            .filter_map(|(item, _)| match item { Item::Function(function) => Some(function.name.clone()), _ => None })
            .collect();
        retain_referenced_functions(&mut prepared, &emitted_functions, false);
        let type_stage = self.modules.iter().map(|(name, module)| (
            name.clone(), if name == &self.selected { prepared.clone() } else { module.clone() },
        )).collect::<Vec<_>>();
        resolve_stage_type_info(&mut prepared, &type_stage, &self.selected, &self.user_modules)?;
        let dependencies = self.producer_dependencies();
        let modules = self.modules.iter().filter(|(name, _)| dependencies.contains(name)).map(|(name, module)| (
            name.clone(), if name == &self.selected { prepared.clone() } else { module.clone() },
        )).collect();
        let expand = ComptimeExpander::with_sibling_request(require_expansion_free, |_| false);
        let _ = link_checked_with(modules, &self.selected, expand, LinkMode::Production,
            Some(&self.user_modules), true, Some(self.owners.clone()))?;
        if let Ok(mut cache) = self.validated_prefixes.lock() {
            if cache.len() < MAX_CACHED_PREFIX_VALIDATIONS {
                cache.insert(key);
            }
        }
        Ok(())
    }

    /// Check the ordinary destination after all source and generated blocks have run.
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
        self.check_expanded_for_entry(prior, &self.selected)
    }

    /// Check a consumer entry against admitted output from one imported
    /// module. The raw bundle, selected producer, owners, and output prefix
    /// stay inside this session; no formatted generated source is reloaded.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn check_expanded_for_entry(
        &self, prior: &[PriorComptimeBlockOutput], entry: &str,
    ) -> Result<(CheckedModule, Module), PipelineError> {
        if !self.user_modules.contains(entry)
            || !self.modules.iter().any(|(name, _)| name == entry) {
            return Err(refuse("comptime destination entry is not a loader-owned source module"));
        }
        if self.next_block_site(prior)?.is_some() {
            return Err(refuse("comptime destination requires every original and generated block output in order"));
        }
        if prior.iter().flat_map(|output| &output.items).any(|(item, _)|
            matches!(item, Item::Function(function) if function.name == "main")) {
            return Err(refuse("comptime destination cannot displace its source main"));
        }
        let (_, original) = self.modules.iter().find(|(name, _)| name == &self.selected)
            .expect("selected raw module retained");
        let (expanded, emitted_origins) = materialize_prior_with_origins(original, prior, true, false);
        let modules = self.modules.iter().map(|(name, module)| (
            name.clone(), if name == &self.selected { expanded.clone() } else { module.clone() },
        )).collect::<Vec<_>>();
        let display = modules.iter().find(|(name, _)| name == entry)
            .expect("destination entry validated").1.clone();
        let mut hash = blake3::Hasher::new();
        hash.update(b"witchy.comptime-expanded-source.v1\0");
        hash.update(&REWRITE_VERSION.to_le_bytes());
        hash.update(&self.original_digest);
        hash.update(&(self.selected.len() as u64).to_le_bytes());
        hash.update(self.selected.as_bytes());
        if entry != self.selected {
            hash.update(b"imported-destination\0");
            hash.update(&(entry.len() as u64).to_le_bytes());
            hash.update(entry.as_bytes());
        }
        hash_prior_items(&mut hash, prior)?;
        for (name, owner) in self.owners.assignments() {
            super::super::static_evaluation::hash_owner_assignment(&mut hash, name, owner);
        }
        let digest = *hash.finalize().as_bytes();
        let expand = ComptimeExpander::with_sibling_request(require_expansion_free, |_| false);
        let (mut checked, typed) = link_checked_with(modules, entry, expand, LinkMode::Production,
            Some(&self.user_modules), true, Some(self.owners.clone()))?;
        retain_emitted_origins(&mut checked, &self.selected, &emitted_origins);
        let typed = typed.ok_or_else(|| refuse("comptime destination did not retain checked source"))?;
        let checked = finish_canonical_source(checked, typed, digest, PrecompiledDeclarationEnvironment::default())?;
        Ok((checked, display))
    }

    /// Seal an entry after independently admitted imported producers have all
    /// finished. The compiler driver supplies actual runtime outputs; this
    /// method checks their complete prefixes and their ordinary destination.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn check_expanded_producer_graph_for_entry(
        &self, outputs: &[(String, Vec<PriorComptimeBlockOutput>)], entry: &str,
    ) -> Result<(CheckedModule, Module), PipelineError> {
        if self.selected != entry || !self.user_modules.contains(entry) {
            return Err(refuse("comptime graph requires the loader-owned entry session"));
        }
        let mut expanded = self.modules.as_ref().clone();
        let mut origins = Vec::new();
        let mut hash = blake3::Hasher::new();
        hash.update(b"witchy.comptime-expanded-graph.v1\0");
        hash.update(&REWRITE_VERSION.to_le_bytes());
        hash.update(&self.original_digest);
        let producers = outputs.iter().map(|(name, _)| name.clone()).collect::<Vec<_>>();
        let mut seen = HashSet::new();
        for (producer, prior) in outputs {
            if !seen.insert(producer.clone()) || !self.user_modules.contains(producer) {
                return Err(refuse("comptime graph requires unique loader-owned producers"));
            }
            let baseline = self.select_module(producer)?;
            let dependencies = baseline.producer_dependency_names(&producers);
            let predecessors = outputs.iter().filter(|(name, _)| dependencies.contains(name)
                && name != producer && seen.contains(name))
                .cloned().collect::<Vec<_>>();
            let session = baseline.with_predecessors(&predecessors)?;
            if session.original_block_items().is_empty() || session.next_block_site(prior)?.is_some() {
                return Err(refuse("comptime graph requires every producer block output in order"));
            }
            session.check_prior_items(prior)?;
            if prior.iter().flat_map(|output| &output.items).any(|(item, _)|
                matches!(item, Item::Function(function) if function.name == "main")) {
                return Err(refuse("comptime destination cannot displace its source main"));
            }
            let (_, original) = session.modules.iter().find(|(name, _)| name == producer)
                .expect("selected raw module retained");
            let (module, emitted_origins) = materialize_prior_with_origins(original, prior, true, false);
            expanded.iter_mut().find(|(name, _)| name == producer)
                .expect("producer raw module retained").1 = module;
            origins.push((producer.clone(), emitted_origins));
            hash.update(&(producer.len() as u64).to_le_bytes());
            hash.update(producer.as_bytes());
            hash_prior_items(&mut hash, prior)?;
        }
        let expected = self.modules.iter().filter(|(name, module)|
            self.user_modules.contains(name) && module.items.iter().any(|item| matches!(item, Item::Comptime(_))))
            .map(|(name, _)| name).collect::<Vec<_>>();
        if expected.len() != outputs.len() || expected.iter().any(|name|
            !outputs.iter().any(|(producer, _)| producer == *name)) {
            return Err(refuse("comptime graph omitted a source producer"));
        }
        for (name, owner) in self.owners.assignments() {
            super::super::static_evaluation::hash_owner_assignment(&mut hash, name, owner);
        }
        let display = expanded.iter().find(|(name, _)| name == entry)
            .expect("entry raw module retained").1.clone();
        let expand = ComptimeExpander::with_sibling_request(require_expansion_free, |_| false);
        let (mut checked, typed) = link_checked_with(expanded, entry, expand, LinkMode::Production,
            Some(&self.user_modules), true, Some(self.owners.clone()))?;
        for (producer, emitted_origins) in &origins {
            retain_emitted_origins(&mut checked, producer, emitted_origins);
        }
        let typed = typed.ok_or_else(|| refuse("comptime destination did not retain checked source"))?;
        let checked = finish_canonical_source(checked, typed, *hash.finalize().as_bytes(),
            PrecompiledDeclarationEnvironment::default())?;
        Ok((checked, display))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn materialized_graph_modules(
        &self, outputs: &[(String, Vec<PriorComptimeBlockOutput>)],
    ) -> Vec<(String, Module)> {
        let mut modules = self.modules.as_ref().clone();
        for (producer, prior) in outputs {
            let (_, original) = self.modules.iter().find(|(name, _)| name == producer)
                .expect("validated graph producer is a raw module");
            modules.iter_mut().find(|(name, _)| name == producer)
                .expect("validated graph producer retained").1 =
                materialize_prior(original, prior, true, false);
        }
        modules
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn check_expanded_producer_graph_build_recipe(
        &self, outputs: &[(String, Vec<PriorComptimeBlockOutput>)], entry: &str,
    ) -> Result<super::super::CheckedBuildRecipe, PipelineError> {
        let (original, _) = self.check_expanded_producer_graph_for_entry(outputs, entry)?;
        let (_, raw_entry) = self.modules.iter().find(|(name, _)| name == entry)
            .expect("validated graph entry retained");
        let mut modules = self.materialized_graph_modules(outputs);
        let expand = ComptimeExpander::with_sibling_request(require_expansion_free, |_| false);
        let admitted = original.canonical_source()
            .ok_or_else(|| refuse("admitted build graph lacks canonical authority"))?;
        let original_digest = checked_source_digest(&self.raw_sources, entry,
            &PrecompiledDeclarationEnvironment::default());
        let (checked, identity) = super::super::build_recipe::finish_build_recipe_from_checked_original(
            &mut modules, entry, expand, &self.user_modules, self.owners.clone(),
            admitted.typed_module(), original_digest, Some(raw_entry),
            Some(admitted.source_witness().source_bundle_digest()),
        )?;
        Ok(super::super::build_recipe::seal_admitted_build_recipe(original, checked, identity))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn check_expanded_producer_graph_test_suite(
        &self, outputs: &[(String, Vec<PriorComptimeBlockOutput>)], entry: &str,
    ) -> Result<super::super::CheckedTestSuite, PipelineError> {
        let (checked, _) = self.check_expanded_producer_graph_for_entry(outputs, entry)?;
        let digest = checked.canonical_source_witness()
            .ok_or_else(|| refuse("admitted test graph lacks canonical authority"))?
            .source_bundle_digest();
        super::super::test_driver::seal_admitted_test_suite(
            self.raw_sources.as_ref().clone(), entry, &self.user_modules,
            self.owners.clone(), self.materialized_graph_modules(outputs), digest,
        )
    }

    /// Derive the sealed build wrapper from the same admitted expansion that
    /// checked the original source. Target selection stays on the raw entry;
    /// emitted syntax may supply helpers but cannot mint a new `build` target.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn check_expanded_build_recipe(
        &self, prior: &[PriorComptimeBlockOutput],
    ) -> Result<super::super::CheckedBuildRecipe, PipelineError> {
        self.check_expanded_build_recipe_for_entry(prior, &self.selected)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn check_expanded_build_recipe_for_entry(
        &self, prior: &[PriorComptimeBlockOutput], entry: &str,
    ) -> Result<super::super::CheckedBuildRecipe, PipelineError> {
        let (original, _) = self.check_expanded_for_entry(prior, entry)?;
        let (_, raw_entry) = self.modules.iter().find(|(name, _)| name == entry)
            .expect("selected raw entry retained");
        let (_, raw_producer) = self.modules.iter().find(|(name, _)| name == &self.selected)
            .expect("selected raw producer retained");
        let expanded = materialize_prior(raw_producer, prior, true, false);
        let mut modules = self.modules.iter().map(|(name, module)| (
            name.clone(), if name == &self.selected { expanded.clone() } else { module.clone() },
        )).collect();
        let expand = ComptimeExpander::with_sibling_request(require_expansion_free, |_| false);
        let admitted = original.canonical_source()
            .ok_or_else(|| refuse("admitted build source lacks canonical authority"))?;
        let original_typed = admitted.typed_module();
        let admitted_digest = admitted.source_witness().source_bundle_digest();
        let original_digest = checked_source_digest(&self.raw_sources, entry,
            &PrecompiledDeclarationEnvironment::default());
        let (checked, identity) = super::super::build_recipe::finish_build_recipe_from_checked_original(
            &mut modules, entry, expand, &self.user_modules, self.owners.clone(),
            original_typed, original_digest, Some(raw_entry), Some(admitted_digest),
        )?;
        Ok(super::super::build_recipe::seal_admitted_build_recipe(original, checked, identity))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn check_expanded_test_suite(
        &self, prior: &[PriorComptimeBlockOutput],
    ) -> Result<super::super::CheckedTestSuite, PipelineError> {
        self.check_expanded_test_suite_for_entry(prior, &self.selected)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn check_expanded_test_suite_for_entry(
        &self, prior: &[PriorComptimeBlockOutput], entry: &str,
    ) -> Result<super::super::CheckedTestSuite, PipelineError> {
        let (checked, _) = self.check_expanded_for_entry(prior, entry)?;
        let digest = checked.canonical_source_witness()
            .ok_or_else(|| refuse("admitted test source lacks canonical authority"))?
            .source_bundle_digest();
        let (_, original) = self.modules.iter().find(|(name, _)| name == &self.selected)
            .expect("selected raw producer retained");
        let expanded = materialize_prior(original, prior, true, false);
        let admitted_modules = self.modules.iter().map(|(name, module)| (
            name.clone(), if name == &self.selected { expanded.clone() } else { module.clone() },
        )).collect();
        super::super::test_driver::seal_admitted_test_suite(
            self.raw_sources.as_ref().clone(), entry, &self.user_modules,
            self.owners.clone(), admitted_modules, digest,
        )
    }
}

/// A compiler recipe executes only its selected block and helpers reachable
/// from that block. Runtime functions elsewhere in the source can require
/// capabilities unavailable to this zero-grant session. The Debug rendering
/// over-approximates names across calls, function values and generated syntax;
/// an omitted real reference still fails closed during ordinary checking.
fn retain_referenced_functions(module: &mut Module, prior_functions: &HashSet<String>, keep_entry: bool) {
    if module.items.is_empty() { return; }
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

fn materialize_prior(
    module: &Module, prior: &[PriorComptimeBlockOutput], keep_main: bool,
    retain_comptime_helpers: bool,
) -> Module {
    materialize_prior_with_origins(module, prior, keep_main, retain_comptime_helpers).0
}

#[cfg(not(target_arch = "wasm32"))]
fn retain_emitted_origins(
    checked: &mut CheckedModule, selected: &str,
    emitted: &[Option<(ComptimeSyntaxOrigin, u32)>],
) {
    use witchy_syntax::origin::{ExpansionOrigin, HoleOrigin, SourceSpan};
    let Some(mapping) = checked.linked.source_item_maps.get(selected) else { return; };
    for (source_index, provenance) in emitted.iter().enumerate() {
        let Some((syntax_origin, site_line)) = provenance else { continue; };
        // Host item constructors use line 0 or 1 when they have no lexical
        // source location. Attribute those items to the original block site.
        let definition_line = if syntax_origin.definition_line > 1 {
            syntax_origin.definition_line
        } else { *site_line };
        let origin = ExpansionOrigin {
            definition: SourceSpan::line(selected, definition_line),
            invocation: SourceSpan::line(selected, *site_line),
            hole_ancestry: syntax_origin.hole_ancestry.iter().map(|hole| HoleOrigin {
                category: hole.category,
                definition: SourceSpan::line(selected, hole.definition_line),
                invocation: SourceSpan::line(selected, hole.invocation_line),
            }).collect(),
        };
        if let Some(targets) = mapping.get(source_index) {
            for &target in targets {
                if checked.linked.origins.origin_for_item(target).is_none() {
                    if let Some(item) = checked.linked.module.items.get(target) {
                        checked.linked.origins.record_item_tree(selected, target, item, origin.clone());
                    }
                }
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn replace_first_tagged_literal(module: &mut Module, replacement: Expr) -> Option<(String, u32)> {
    let mut replacement = Some(replacement);
    let mut selected = None;
    let mut visit = |expression: &mut Expr| {
        if let Expr::TaggedLit { tag, line, .. } = expression {
            if let Some(value) = replacement.take() {
                selected = Some((tag.clone(), *line));
                *expression = value;
            }
        }
        Ok::<_, ()>(())
    };
    for item in &mut module.items {
        match item {
            Item::Function(function) => witchy_syntax::ast::visit::visit_block_mut(&mut function.body, &mut visit),
            Item::Impl(definition) => {
                for method in &mut definition.methods {
                    witchy_syntax::ast::visit::visit_block_mut(&mut method.body, &mut visit)
                        .expect("tag replacement traversal cannot fail");
                }
                Ok(())
            }
            Item::Trait(definition) => {
                for method in &mut definition.methods {
                    if let Some(body) = &mut method.default {
                        witchy_syntax::ast::visit::visit_block_mut(body, &mut visit)
                            .expect("tag replacement traversal cannot fail");
                    }
                }
                Ok(())
            }
            Item::Const { value, .. } => witchy_syntax::ast::visit::visit_expr_mut(value, &mut visit),
            Item::Comptime(block) => witchy_syntax::ast::visit::visit_block_mut(block, &mut visit),
            Item::Type(_) | Item::TypeAlias { .. } => Ok(()),
        }.expect("tag replacement traversal cannot fail");
    }
    selected
}

fn materialize_prior_with_origins(
    module: &Module, prior: &[PriorComptimeBlockOutput], keep_main: bool,
    retain_comptime_helpers: bool,
) -> (Module, Vec<Option<(ComptimeSyntaxOrigin, u32)>>) {
    fn append_output(
        prepared: &mut Module, prior: &[PriorComptimeBlockOutput], cursor: &mut usize,
        site: ComptimeBlockSite, retain_comptime_helpers: bool,
        site_line: u32, origins: &mut Vec<Option<(ComptimeSyntaxOrigin, u32)>>,
    ) {
        let Some(output) = prior.get(*cursor).filter(|output| output.site == site) else { return; };
        let parent_output = *cursor;
        *cursor += 1;
        for (emitted_item, (generated, origin)) in output.items.iter().enumerate() {
            if matches!(generated, Item::Comptime(_)) {
                append_output(prepared, prior, cursor,
                    ComptimeBlockSite::Generated { parent_output, emitted_item }, retain_comptime_helpers,
                    origin.definition_line.max(site_line), origins);
            } else if retain_comptime_helpers
                || !matches!(generated, Item::Function(function) if function.comptime_only) {
                prepared.items.push(generated.clone());
                prepared.item_lines.push(origin.definition_line);
                origins.push(Some((origin.clone(), site_line)));
            }
        }
    }
    let mut prepared = module.clone();
    prepared.items.clear();
    prepared.item_lines.clear();
    let mut origins = Vec::new();
    let mut cursor = 0;
    for (index, original) in module.items.iter().enumerate() {
        if matches!(original, Item::Comptime(_)) {
            append_output(&mut prepared, prior, &mut cursor,
                ComptimeBlockSite::Original(index), retain_comptime_helpers,
                module.item_lines.get(index).copied().unwrap_or(0), &mut origins);
        } else if (keep_main || !matches!(original, Item::Function(function) if function.name == "main"))
            && (retain_comptime_helpers || !matches!(original, Item::Function(function) if function.comptime_only)) {
            prepared.items.push(original.clone());
            prepared.item_lines.push(module.item_lines.get(index).copied().unwrap_or(0));
            origins.push(None);
        }
    }
    (prepared, origins)
}

fn source_bundle_context_digest(sources: &[(String, Vec<u8>)]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"witchy.comptime-source-bundle-context.v1\0");
    for (name, source) in sources {
        hash.update(&(name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update(&(source.len() as u64).to_le_bytes());
        hash.update(source);
    }
    *hash.finalize().as_bytes()
}

fn prefix_validation_key(
    source_context: [u8; 32], selected: &str, prior: &[PriorComptimeBlockOutput],
) -> Result<PrefixValidationKey, PipelineError> {
    let mut hash = blake3::Hasher::new();
    hash.update(b"witchy.comptime-prefix-validation.v1\0");
    hash.update(&source_context);
    hash.update(&(selected.len() as u64).to_le_bytes());
    hash.update(selected.as_bytes());
    hash_prior_items(&mut hash, prior)?;
    Ok(PrefixValidationKey {
        source_context,
        selected: selected.into(),
        prior_output: *hash.finalize().as_bytes(),
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn hash_prior_items(hash: &mut blake3::Hasher, prior: &[PriorComptimeBlockOutput]) -> Result<(), PipelineError> {
    hash.update(&(prior.len() as u64).to_le_bytes());
    for output in prior {
        hash_block_site(hash, output.site);
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

fn hash_block_site(hash: &mut blake3::Hasher, site: ComptimeBlockSite) {
    match site {
        ComptimeBlockSite::Original(item) => {
            hash.update(&[0]);
            hash.update(&(item as u64).to_le_bytes());
        }
        ComptimeBlockSite::Generated { parent_output, emitted_item } => {
            hash.update(&[1]);
            hash.update(&(parent_output as u64).to_le_bytes());
            hash.update(&(emitted_item as u64).to_le_bytes());
        }
    }
}

// This fixed callback never mutates any module. A public callback here could
// replace the selected body or execute sibling comptime against unrecorded
// prior output. Pending expansion instead stays outside this first-entry lane.
pub fn require_expansion_free(
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
