//! Compiler-owned sequencing of original compile-time blocks. No caller can
//! insert generated syntax between steps: every prefix comes from an admitted
//! zero-grant Wasm execution of the exact recipe selected from raw source.

use std::collections::HashSet;
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

/// Compile original blocks in source order, then check their generated items
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

fn run_source_bundle(
    sources: Vec<(String, Vec<u8>)>, selected: &str,
    user_modules: HashSet<String>, owners: AuthenticatedModuleOwners,
    limits: ComptimeLimits,
) -> Result<CheckedExpansion, String> {
    let session = ComptimeSourceSession::from_source_bundle(sources, selected, user_modules, owners)
        .map_err(|error| error.to_string())?;
    let mut prior = Vec::new();
    for original_item in session.original_block_items() {
        let recipe = session.check_block_with_prior_output(original_item, &prior)
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
            original_item,
            items: outcome.items.into_iter().map(|emitted| (emitted.item, emitted.origin)).collect(),
        });
    }
    let (checked, selected_module) = session.check_expanded_with_display_module(&prior)
        .map_err(|error| error.to_string())?;
    Ok(CheckedExpansion { checked, selected_module })
}
