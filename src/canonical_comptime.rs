//! Compiler-owned sequencing of source and generated compile-time blocks. No caller can
//! insert generated syntax between steps: every prefix comes from an admitted
//! zero-grant Wasm execution of the exact recipe selected from raw source.

use std::collections::{HashMap, HashSet};
use witchy_lower::codegen::{self, LoweringOutcome};
use witchy_runtime::runtime::{Runtime, comptime::ComptimeLimits};
use witchy_types::pipeline::{CheckedModule, ComptimeSourceSession, PriorComptimeBlockOutput};
use witchy_types::runtime_type::AuthenticatedModuleOwners;
use witchy_syntax::ast::Module;

struct CheckedExpansion {
    checked: CheckedModule,
    selected_module: Module,
}

/// Fixed compiler-service limits for production source loading. Each original
/// block receives a fresh zero-grant session with these independent ceilings.
pub fn source_limits() -> ComptimeLimits {
    ComptimeLimits {
        fuel: 50_000_000, memory_pages: 256, fresh_identifiers: 4096,
        host_output_bytes: 1_048_576, syntax_nodes: 100_000,
        syntax_bytes: 1_048_576, emitted_nodes: 4096,
        emitted_bytes: 1_048_576,
    }
}

/// Compile source and generated blocks in source order, then check their items
/// in the ordinary destination. The loader remains responsible for obtaining
/// raw bytes and owners; this function accepts neither an AST nor a prior
/// output or digest from its caller. Unsupported emission fails closed.
pub fn check_source_bundle(
    sources: Vec<(String, Vec<u8>)>, selected: &str,
    user_modules: HashSet<String>, owners: AuthenticatedModuleOwners,
    limits: ComptimeLimits,
) -> Result<CheckedModule, String> {
    run_source_bundle(sources, selected, user_modules, owners, limits).map(|output| output.checked)
}

/// Execute a source-defined, hole-free tagged literal after admitted blocks,
/// then recheck the actual emitted expression in its original destination.
pub fn check_source_bundle_with_local_tag(
    sources: Vec<(String, Vec<u8>)>, selected: &str,
    user_modules: HashSet<String>, owners: AuthenticatedModuleOwners,
    limits: ComptimeLimits,
) -> Result<CheckedModule, String> {
    let session = ComptimeSourceSession::from_source_bundle(sources, selected, user_modules, owners)
        .map_err(|error| error.to_string())?;
    let prior = execute_source_blocks(&session, limits)?;
    let recipe = session.check_first_local_tag_recipe(&prior)
        .map_err(|error| error.to_string())?;
    let digest = recipe.session_source_digest();
    let artifact = match codegen::compile_checked_comptime_recipe_artifact(&recipe) {
        LoweringOutcome::Lowered(artifact) => artifact,
        LoweringOutcome::Unsupported(reason) => return Err(reason.to_string()),
        LoweringOutcome::Rejected(error) => return Err(error.to_string()),
    };
    let mut outcome = Runtime::run_comptime(&artifact, limits).map_err(|error| error.to_string())?;
    if outcome.source_digest() != digest {
        return Err("tag session returned output for a different admitted source recipe".into());
    }
    if !outcome.items.is_empty() || outcome.expressions.len() != 1 {
        return Err("local tag must emit exactly one expression and no items".into());
    }
    let emitted = outcome.expressions.remove(0);
    session.check_expanded_first_local_tag_output(&prior, &recipe, emitted.expression, emitted.origin)
        .map_err(|error| error.to_string())
}

/// Execute source and generated blocks in one imported module, then check the
/// loader's actual entry against their admitted output. No generated source
/// bytes or caller-provided AST cross the source-session boundary.
pub fn check_source_bundle_with_imported_blocks(
    sources: Vec<(String, Vec<u8>)>, entry: &str, selected: &str,
    user_modules: HashSet<String>, owners: AuthenticatedModuleOwners,
    limits: ComptimeLimits,
) -> Result<CheckedModule, String> {
    run_source_bundle_for_entry(sources, entry, selected, user_modules, owners, limits)
        .map(|output| output.checked)
}

pub fn check_build_recipe_source_bundle(
    sources: Vec<(String, Vec<u8>)>, entry: &str,
    user_modules: HashSet<String>, owners: AuthenticatedModuleOwners,
    limits: ComptimeLimits,
) -> Result<witchy_types::pipeline::CheckedBuildRecipe, String> {
    let session = ComptimeSourceSession::from_source_bundle(sources, entry, user_modules, owners)
        .map_err(|error| error.to_string())?;
    let prior = execute_source_blocks(&session, limits)?;
    session.check_expanded_build_recipe(&prior).map_err(|error| error.to_string())
}

pub fn check_build_recipe_source_bundle_with_imported_blocks(
    sources: Vec<(String, Vec<u8>)>, entry: &str, selected: &str,
    user_modules: HashSet<String>, owners: AuthenticatedModuleOwners,
    limits: ComptimeLimits,
) -> Result<witchy_types::pipeline::CheckedBuildRecipe, String> {
    let session = ComptimeSourceSession::from_source_bundle(sources, selected, user_modules, owners)
        .map_err(|error| error.to_string())?;
    let prior = execute_source_blocks(&session, limits)?;
    session.check_expanded_build_recipe_for_entry(&prior, entry)
        .map_err(|error| error.to_string())
}

pub fn check_test_suite_source_bundle(
    sources: Vec<(String, Vec<u8>)>, entry: &str,
    user_modules: HashSet<String>, owners: AuthenticatedModuleOwners,
    limits: ComptimeLimits,
) -> Result<witchy_types::pipeline::CheckedTestSuite, String> {
    let session = ComptimeSourceSession::from_source_bundle(sources, entry, user_modules, owners)
        .map_err(|error| error.to_string())?;
    let prior = execute_source_blocks(&session, limits)?;
    session.check_expanded_test_suite(&prior).map_err(|error| error.to_string())
}

pub fn check_test_suite_source_bundle_with_imported_blocks(
    sources: Vec<(String, Vec<u8>)>, entry: &str, selected: &str,
    user_modules: HashSet<String>, owners: AuthenticatedModuleOwners,
    limits: ComptimeLimits,
) -> Result<witchy_types::pipeline::CheckedTestSuite, String> {
    let session = ComptimeSourceSession::from_source_bundle(sources, selected, user_modules, owners)
        .map_err(|error| error.to_string())?;
    let prior = execute_source_blocks(&session, limits)?;
    session.check_expanded_test_suite_for_entry(&prior, entry)
        .map_err(|error| error.to_string())
}

pub fn check_and_expand_source_bundle_with_imported_blocks(
    sources: Vec<(String, Vec<u8>)>, entry: &str, selected: &str,
    user_modules: HashSet<String>, owners: AuthenticatedModuleOwners,
    limits: ComptimeLimits,
) -> Result<(CheckedModule, Module), String> {
    run_source_bundle_for_entry(sources, entry, selected, user_modules, owners, limits)
        .map(|output| (output.checked, output.selected_module))
}

/// Return only source-shaped expansion produced by admitted execution and
/// accepted by ordinary destination checking. This is presentation data, not
/// a checked-source witness that can be submitted to lowering.
pub fn expand_source_bundle(
    sources: Vec<(String, Vec<u8>)>, selected: &str,
    user_modules: HashSet<String>, owners: AuthenticatedModuleOwners,
    limits: ComptimeLimits,
) -> Result<Module, String> {
    run_source_bundle(sources, selected, user_modules, owners, limits)
        .map(|output| output.selected_module)
}

/// Give source inspection the checked destination and its exact displayed
/// entry from one admitted compiler session. The module is presentation data;
/// only the checked result carries canonical admission authority.
pub fn check_and_expand_source_bundle(
    sources: Vec<(String, Vec<u8>)>, selected: &str,
    user_modules: HashSet<String>, owners: AuthenticatedModuleOwners,
    limits: ComptimeLimits,
) -> Result<(CheckedModule, Module), String> {
    run_source_bundle(sources, selected, user_modules, owners, limits)
        .map(|output| (output.checked, output.selected_module))
}

fn run_source_bundle(
    sources: Vec<(String, Vec<u8>)>, selected: &str,
    user_modules: HashSet<String>, owners: AuthenticatedModuleOwners,
    limits: ComptimeLimits,
) -> Result<CheckedExpansion, String> {
    run_source_bundle_for_entry(sources, selected, selected, user_modules, owners, limits)
}

fn run_source_bundle_for_entry(
    sources: Vec<(String, Vec<u8>)>, entry: &str, selected: &str,
    user_modules: HashSet<String>, owners: AuthenticatedModuleOwners,
    limits: ComptimeLimits,
) -> Result<CheckedExpansion, String> {
    with_std_pruning_fallback(|| run_source_bundle_for_entry_attempt(
        sources.clone(), entry, selected, user_modules.clone(), owners.clone(), limits,
    ))
}

fn run_source_bundle_for_entry_attempt(
    sources: Vec<(String, Vec<u8>)>, entry: &str, selected: &str,
    user_modules: HashSet<String>, owners: AuthenticatedModuleOwners,
    limits: ComptimeLimits,
) -> Result<CheckedExpansion, String> {
    let session = ComptimeSourceSession::from_source_bundle(sources, selected, user_modules, owners)
        .map_err(|error| error.to_string())?;
    let prior = execute_source_blocks(&session, limits)?;
    let (checked, selected_module) = session.check_expanded_for_entry(&prior, entry)
        .map_err(|error| error.to_string())?;
    Ok(CheckedExpansion { checked, selected_module })
}

fn with_std_pruning_fallback<T>(mut run: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    let (pruned_result, pruned) = witchy_types::std_pruning::with_pruning(true, || run());
    if pruned_result.is_ok() || !pruned {
        witchy_types::std_pruning::log(if pruned { "pruned-ok" } else { "not-pruned" }.into());
        return pruned_result;
    }
    if let Err(error) = &pruned_result {
        witchy_types::std_pruning::log(format!(
            "fallback: {}", error.lines().next().unwrap_or(""),
        ));
    }
    witchy_types::std_pruning::with_pruning(false, || run()).0
}

/// Execute imported producers in dependency order from the authenticated raw
/// bundle. Every successor recipe binds admitted predecessor syntax and its
/// digest before execution; the final destination replays that same chain.
pub fn check_and_expand_source_bundle_with_producer_graph(
    sources: Vec<(String, Vec<u8>)>, entry: &str, producers: &[String],
    user_modules: HashSet<String>, owners: AuthenticatedModuleOwners,
    limits: ComptimeLimits,
) -> Result<(CheckedModule, Module), String> {
    let (entry_session, outputs) = execute_source_producer_graph(
        sources, entry, producers, user_modules, owners, limits,
    )?;
    entry_session.check_expanded_producer_graph_for_entry(&outputs, entry)
        .map_err(|error| error.to_string())
}

pub fn check_build_recipe_source_bundle_with_producer_graph(
    sources: Vec<(String, Vec<u8>)>, entry: &str, producers: &[String],
    user_modules: HashSet<String>, owners: AuthenticatedModuleOwners,
    limits: ComptimeLimits,
) -> Result<witchy_types::pipeline::CheckedBuildRecipe, String> {
    let (session, outputs) = execute_source_producer_graph(
        sources, entry, producers, user_modules, owners, limits,
    )?;
    session.check_expanded_producer_graph_build_recipe(&outputs, entry)
        .map_err(|error| error.to_string())
}

pub fn check_test_suite_source_bundle_with_producer_graph(
    sources: Vec<(String, Vec<u8>)>, entry: &str, producers: &[String],
    user_modules: HashSet<String>, owners: AuthenticatedModuleOwners,
    limits: ComptimeLimits,
) -> Result<witchy_types::pipeline::CheckedTestSuite, String> {
    let (session, outputs) = execute_source_producer_graph(
        sources, entry, producers, user_modules, owners, limits,
    )?;
    session.check_expanded_producer_graph_test_suite(&outputs, entry)
        .map_err(|error| error.to_string())
}

fn execute_source_producer_graph(
    sources: Vec<(String, Vec<u8>)>, entry: &str, producers: &[String],
    user_modules: HashSet<String>, owners: AuthenticatedModuleOwners,
    limits: ComptimeLimits,
) -> Result<(ComptimeSourceSession, Vec<(String, Vec<PriorComptimeBlockOutput>)>), String> {
    let entry_session = ComptimeSourceSession::from_source_bundle(
        sources, entry, user_modules, owners,
    ).map_err(|error| error.to_string())?;
    let mut outputs = Vec::new();
    let mut pending = producers.to_vec();
    pending.sort();
    if pending.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("comptime graph has duplicate producers".into());
    }
    let mut dependency_cache = HashMap::<String, Vec<String>>::new();
    let mut completed = HashSet::new();
    while !pending.is_empty() {
        let mut ready = None;
        for (index, producer) in pending.iter().enumerate() {
            if !dependency_cache.contains_key(producer) {
                let baseline = entry_session.select_module(producer)
                    .map_err(|error| error.to_string())?;
                let dependencies = baseline.producer_dependency_names(producers);
                dependency_cache.insert(producer.clone(), dependencies.clone());
            }
            if dependency_cache[producer].iter().all(|name| completed.contains(name)) {
                ready = Some(index);
                break;
            }
        }
        let Some(index) = ready else {
            return Err("comptime producer import graph is cyclic".into());
        };
        let producer = pending.remove(index);
        let dependencies = &dependency_cache[&producer];
        let predecessors = outputs.iter().filter(|(name, _)| dependencies.contains(name))
            .cloned().collect::<Vec<_>>();
        let session = entry_session.select_module(&producer)
            .map_err(|error| error.to_string())?
            .with_predecessors(&predecessors)
            .map_err(|error| error.to_string())?;
        let prior = execute_source_blocks(&session, limits)?;
        session.check_completed_prefix(&prior).map_err(|error| error.to_string())?;
        completed.insert(producer.clone());
        outputs.push((producer, prior));
    }
    Ok((entry_session, outputs))
}

fn execute_source_blocks(
    session: &ComptimeSourceSession, limits: ComptimeLimits,
) -> Result<Vec<PriorComptimeBlockOutput>, String> {
    let mut prior = Vec::new();
    while let Some(site) = session.next_block_site(&prior).map_err(|error| error.to_string())? {
        if prior.len() >= 256 {
            return Err("comptime source session exceeded its 256-block execution limit".into());
        }
        let recipe = session.check_next_block_with_prior_output(&prior)
            .map_err(|error| error.to_string())?;
        let expected_digest = recipe.session_source_digest();
        let artifact = match codegen::compile_checked_comptime_recipe_artifact(&recipe) {
            LoweringOutcome::Lowered(artifact) => artifact,
            LoweringOutcome::Unsupported(reason) => return Err(reason.to_string()),
            LoweringOutcome::Rejected(error) => return Err(error.to_string()),
        };
        let outcome = Runtime::run_comptime(&artifact, limits).map_err(|error| error.to_string())?;
        if outcome.source_digest() != expected_digest {
            return Err("compiler session returned output for a different admitted source recipe".into());
        }
        if !outcome.expressions.is_empty() {
            return Err("compiler session expression outputs require a typed destination site".into());
        }
        prior.push(PriorComptimeBlockOutput {
            site,
            items: outcome.items.into_iter().map(|emitted| (emitted.item, emitted.origin)).collect(),
        });
    }
    Ok(prior)
}
