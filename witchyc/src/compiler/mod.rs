//! Type checking, proof admission, and native and WebAssembly code generation.
//! The typed program is private to the compiler.
mod package_tests;
pub use package_tests::{build_package_tests,check_test_consumer};
mod parallel;
/// The stack a thread that compiles is given (`witchyc` runs on one). The parser's limits on
/// nesting are what stop a deep program, never the stack; only what is used is touched.
pub const COMPILER_STACK: usize = parallel::STACK;
mod guarded;
mod host;
mod effects;
mod pause;
mod traps;
mod caps;
mod grant_document;
mod exec_env_policy;
mod spec;
mod using;
mod spec_expr;
mod spec_resolve;
mod spec_expand;
#[cfg(test)]
mod spec_tests;
mod borrow;
mod operations;
mod logical_body;
mod logical_place;
mod semantic_types;
mod concrete;
#[cfg(all(target_arch = "aarch64", any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
mod native;
mod instances;
mod call_graph;
mod contract;
mod storage_contract;
mod memory_contract;
mod memory_model;
#[cfg(test)]
mod memory_tests;
#[cfg(test)]
mod any_tests;
#[cfg(test)]
mod anonymous_tests;
#[cfg(test)]
mod sealed_tests;
#[cfg(test)]
mod brand_tests;
#[cfg(test)]
mod early_exit_tests;
#[cfg(test)]
mod inference_tests;
#[cfg(test)]
mod sum_tests;
#[cfg(test)]
mod rearrangement_tests;
#[cfg(test)]
mod union_tests;
#[cfg(test)]
mod methods_tests;
#[cfg(test)]
mod references_tests;
#[cfg(test)]
mod unique_reference_tests;
#[cfg(test)]
mod through_reference_tests;
#[cfg(test)]
mod match_unique_tests;
#[cfg(test)]
mod signed_tests;
#[cfg(test)]
mod view_tests;
#[cfg(test)]
mod borrow_safety_tests;
#[cfg(test)]
mod iteration_tests;
#[cfg(test)]
mod unchecked_read_tests;
#[cfg(test)]
mod traits_tests;
#[cfg(test)]
mod invariant_tests;
mod storage_model;
mod storage_vc;
mod record_contract;
mod record_value;
mod record_focus;
mod record_caller;
mod record_post;
mod contract_sites;
mod record_state;
mod record_model;
mod record_memory;
mod source_vc;
pub(crate) mod proof_advice;
mod scalar_vc;
mod indices;
mod allocator_code;
mod allocator;
mod buffer;
mod properties;
mod flow;
mod sum;
mod match_arm;
mod vacant_store;
mod rearrangement;
mod methods;
// The key law from the shape of Eq, HashKey and Equivalent bodies (docs/plan-derive-laws.md).
mod key_law;
mod early_exit;
mod nested_patterns;
mod iteration;
mod inline_generator;
mod lending;
mod cell;
mod place;
mod frame;
mod check;
mod emit;
mod inline;
mod forward;
mod files;
mod vector_object;
pub use vector_object::build_object;
mod compiler_session;
pub use compiler_session::serve;
macro_rules! compiler_diagnostic {
    ($($arg:tt)*) => { $crate::compiler::compiler_session::diagnostic(format_args!($($arg)*)) };
}
pub(super) use compiler_diagnostic;
mod ready;
mod build;
mod build_signal;
pub use build::{BuildSession, loop_build as build_session, loop_target as build_target_session, watch as watch_build};
pub use allocator::{Heap, configure as configure_heap};
mod ir;
mod layout;
mod proof;
mod proof_module;
mod proof_body;
mod proof_reach;
mod proof_cache;
mod proof_session;
mod proof_wire2;
mod proof_measure;
mod proof_digest;
mod scalar;

use crate::diagnostic::Diagnostic;
use std::time::Instant;

pub const PROFILE: &str = "witchy.scalar.v1";
pub const BUFFER_PROFILE: &str = "witchy.buffer.v2";
pub const FRAME_PROFILE: &str = "witchy.frame.v1";
/// The size limit of one generated proof statement and of similar bounded texts.
pub const MAX_SOURCE_BYTES: usize = 65_536;
/// The size limit of a program's source with everything it imports.
pub const MAX_PROGRAM_BYTES: usize = 1 << 20;
pub const MAX_MODULE_BYTES: usize = 16 << 20;

/// All declarations are selected and checked, including unused functions and
/// unreachable statements. Pure scalar lemmas are fresh-checked; unsupported
/// proof obligations fail closed.
pub fn compile(source: &str) -> Result<Vec<u8>, Diagnostic> {
    Ok(compile_report(source)?.0)
}

/// This receipt describes the narrow scalar check, not Lean proof admission.
/// Consumers must bind it to trusted compiler bytes and the exact source/module.
pub fn compile_report(source: &str) -> Result<(Vec<u8>, String), Diagnostic> {
    let (bytes, report, _) = compile_measured(source)?;
    Ok((bytes, report))
}

/// Phase observations, not part of deterministic admission/binding data.
#[derive(Default, Debug)]
pub struct Measurements {
    pub read_ns: u128,
    pub parse_ns: u128,
    pub check_ns: u128,
    pub proof_check_ns: u128,
    pub emit_ns: u128,
    pub receipt_ns: u128,
    pub publish_ns: u128,
    pub source_bytes: usize,
    pub wasm_bytes: usize,
    pub lean_invocations: usize,
    primary_typed_check_ns: u128,
    vc_generation_ns: u128,
    concrete_planning_ns: u128,
    proof: proof_measure::Measurements,
}
impl Measurements {
    pub fn json(&self) -> String {
        format!(
            "{{\"schema\":\"witchy.scalar.measure.v1\",\"unit\":\"nanoseconds\",\"read\":{},\"parse\":{},\"static_check\":{},\"fresh_proof_check\":{},\"emit\":{},\"receipt\":{},\"publish\":{},\"source_bytes\":{},\"wasm_bytes\":{},\"lean_invocations\":{},\"debug_assertions\":{},\"phases\":{{\"static_check\":{{\"primary_typed_check\":{},\"vc_generation\":{},\"concrete_planning\":{}}},\"fresh_proof_check\":{}}}}}",
            self.read_ns,
            self.parse_ns,
            self.check_ns,
            self.proof_check_ns,
            self.emit_ns,
            self.receipt_ns,
            self.publish_ns,
            self.source_bytes,
            self.wasm_bytes,
            self.lean_invocations,
            cfg!(debug_assertions),
            self.primary_typed_check_ns, self.vc_generation_ns, self.concrete_planning_ns, self.proof.json()
        )
    }
}

pub fn compile_measured(source: &str) -> Result<(Vec<u8>, String, Measurements), Diagnostic> {
    let mut session = proof_session::Session::publishing()?;
    let result = compile_backend(source, &[], session.as_mut());
    if let Some(session) = session.as_mut() {
        if result.is_err() { session.refuse(); }
        else { session.validate_publication()?; session.finish()?; }
    }
    result
}
/// Parsed source as this backend reads it: methods are already functions.
/// One file's source, with no file bounds: tests and single-file tools.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn parse(source: &str) -> Result<crate::ast::Program, Diagnostic> { methods::lift(crate::parser::parse(source)?) }
/// A test library's declarations as they would be written to be imported: types, traits,
/// functions and methods (outside trait blocks) marked `pub`.
#[cfg(test)]
pub(crate) fn exported(text: &str) -> String {
    let mut trait_block = false;
    text.lines().map(|line| {
        if !line.starts_with(' ') && !line.is_empty() { trait_block = line.starts_with("trait ") || (line.starts_with("impl") && line.contains(" for ")); }
        if line.starts_with("fn ") || line.starts_with("type ") || line.starts_with("sealed type ") || line.starts_with("trait ") { format!("pub {line}\n") }
        else if line.starts_with("    fn ") && !trait_block { format!("    pub {}\n", &line[4..]) }
        else { format!("{line}\n") }
    }).collect()
}
pub(crate) const VECTOR_LIBRARY: &str = include_str!("../../library/wasm-vec.wy");

pub(crate) fn parse_modules(source: &str, modules: &[usize]) -> Result<crate::ast::Program, Diagnostic> { methods::lift(crate::parser::parse_modules(source, modules)?) }
/// What a build checks of the program once it is typed, before proving: the policy kept beside
/// it (files.rs), given the parsed and checked program the build itself made.
type Gate<'a> = &'a dyn Fn(&crate::ast::Program, &logical_body::Program) -> Result<(), String>;
fn compile_backend(source: &str, modules: &[usize], session: Option<&mut proof_session::Session>) -> Result<(Vec<u8>, String, Measurements), Diagnostic> {
    compile_gated(source, modules, session, None, None)
}
fn analyze_gated(source: &str, modules: &[usize], session: Option<&mut proof_session::Session>,
    gate: Option<Gate>, reviewed: Option<std::sync::Arc<compiler_session::CheckedSource>>,
    target: crate::parser::Target,
) -> Result<(std::sync::Arc<compiler_session::CheckedSource>, Measurements), Diagnostic> {
    if source.len() > MAX_PROGRAM_BYTES {
        return Err(Diagnostic::new(
            "W0400",
            0..source.len(),
            "source exceeds 1 MiB",
        ));
    }
    if let Some(checker) = session.filter(|s| s.production()) {
        let identity = proof_digest::bytes(source.as_bytes()).map_err(|_| Diagnostic::new("W0402", 0..0, "build source identity I/O"))?;
        checker.request("build", &identity);
    }
    let mut measurements = Measurements {
        source_bytes: source.len(),
        ..Measurements::default()
    };
    let start = Instant::now();
    let (checked_source, parse_ns) = match reviewed {
        Some(checked) => (checked, 0),
        None => compiler_session::analyze_target(source, modules, target)?,
    };
    let (syntax, logical) = (&checked_source.syntax, &checked_source.logical);
    measurements.parse_ns = parse_ns;
    let check_ns = start.elapsed().as_nanos().saturating_sub(parse_ns);
    let phase = Instant::now();
    // A policy refusal is reported where the policy is, not in the source.
    if let Some(gate) = gate { gate(syntax, logical).map_err(|message| Diagnostic::new("W0100", 0..0, message))?; }
    measurements.primary_typed_check_ns = check_ns + phase.elapsed().as_nanos();
    Ok((checked_source, measurements))
}

fn compile_gated(source: &str, modules: &[usize], mut session: Option<&mut proof_session::Session>, gate: Option<Gate>, reviewed: Option<std::sync::Arc<compiler_session::CheckedSource>>) -> Result<(Vec<u8>, String, Measurements), Diagnostic> {
    let (checked_source, mut measurements) = analyze_gated(source, modules, session.as_deref_mut(), gate, reviewed, crate::parser::Target::Wasm)?;
    let (syntax, logical) = (&checked_source.syntax, &checked_source.logical);
    // The captured code has no proof authority. Recheck the current request's
    // obligations and policy before returning even byte-identical machine code.
    // An audit build is never taken from, or kept as, a session's image of the ordinary build.
    let cache_image = syntax.test_adapter.is_none() && !vacant_store::audit();
    if cache_image && let Some(image) = compiler_session::image(source, modules) {
        let start = Instant::now();
        let (goals, left_out) = image.unreached.select(checked_source.prepared()?.clone());
        let proofs = verify_build(syntax, source, goals, session, &left_out)?;
        let report = image.report(&proofs);
        measurements.proof_check_ns = start.elapsed().as_nanos();
        measurements.lean_invocations = proofs.invocations;
        measurements.proof = proofs.measurements;
        measurements.wasm_bytes = image.wasm.len();
        return Ok((image.wasm.clone(), report, measurements));
    }
    let tracked = logical.functions.iter().filter(|f| syntax.coverage_modules.contains(&f.module))
        .map(|f| f.definition.0).collect();
    let (syntax, mut checked, proofs, unreached) = check_wasm(source, checked_source, session, true, &mut measurements)?;
    let start = Instant::now();
    if !vacant_store::audit() { vacant_store::plain(&mut checked); }
    let indices = indices::Indices::new(&checked);
    let (bytes, features) = emit::module(&checked, &indices, &tracked)?;
    measurements.emit_ns += start.elapsed().as_nanos();
    measurements.wasm_bytes = bytes.len();
    let start = Instant::now();
    if bytes.len() > MAX_MODULE_BYTES {
        return Err(Diagnostic::new(
            "W0400",
            0..source.len(),
            "module exceeds 16 MiB",
        ));
    }
    // Inventory completed checker results, rather than treating an empty VC
    // list as evidence of checking. Runtime contracts/unsafe remain refused;
    // every selected pure lemma is checked and inventoried separately below.
    let selected = checked.selected_definitions.iter().map(|(name,_)|format!("\"{name}\"")).collect::<Vec<_>>().join(",");
    let coverage = checked.selected_definitions.iter().map(|(name,nodes)|
        format!("{{\"function\":\"{name}\",\"checked_nodes\":{nodes}}}")).collect::<Vec<_>>().join(",");
    let selected_types = syntax
        .declarations
        .iter()
        .filter_map(|declaration| {
            if let crate::ast::Declaration::Record { name, .. } = declaration {
                Some(format!("\"{name}\""))
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    let layouts = scalar::Scalar::ALL.into_iter().map(|scalar| {
        let layout = scalar.layout();
        format!("{{\"name\":\"{}\",\"bits\":{},\"storage_bytes\":{},\"alignment\":{},\"carrier\":\"i64\"}}", layout.name, layout.bits, layout.size, layout.alignment)
    }).collect::<Vec<_>>().join(",");
    let aggregates = checked.layouts.receipt();
    let templates = checked.layouts.templates_receipt()
        .map(|value| format!(",\"generic_record_templates\":{value}"))
        .unwrap_or_default();
    let heap = checked.layouts.uses_heap();
    let frame_profile = if heap || checked.frame_profile() {
        let profile = if heap { BUFFER_PROFILE } else { FRAME_PROFILE };
        format!(",\"runtime_profile\":\"{profile}\",\"frame_memory\":{}", frame::memory_receipt(heap))
    } else { String::new() };
    let sums = checked.layouts.sums_receipt().map(|value| format!(",\"sum_types\":{value}"))
        .unwrap_or_default();
    let capabilities = checked.layouts.capabilities_receipt()
        .map(|value| format!(",\"capability_types\":{value}")).unwrap_or_default();
    let buffers = checked.layouts.buffers_receipt()
        .map(|value| format!(",\"buffer_types\":{value}")).unwrap_or_default();
    let semantic_body = &checked.semantic_receipt;
    let function_instances = &checked.instance_receipt;
    let allocator = if heap { format!(",\"allocator\":{}", allocator::receipt(&checked, &indices)) } else { String::new() };
    let prefix = format!(
        "{{\"schema\":\"witchy.scalar.check.v1\",\"selected_functions\":[{selected}],\"selected_types\":[{selected_types}],\"scalar_layouts\":[{layouts}],\"aggregate_layouts\":{aggregates},\"all_selected_checked\":true,\"coverage\":[{coverage}]"
    );
    let edge_map = if syntax.coverage_modules.is_empty() { String::new() } else {
        format!(",\"guest_edges_v1\":[{}]",features.join(","))
    };
    let suffix = format!(
        "{edge_map}{templates}{frame_profile}{sums}{capabilities}{buffers}{allocator},\"semantic_body\":{semantic_body},\"function_instances\":{function_instances},\"output_required\":{}}}", checked.output);
    let report = compiler_session::receipt(&prefix, &suffix, &proofs.inventory, &proofs.fresh);
    if cache_image && compiler_session::active() {
        compiler_session::remember(source, modules, compiler_session::Image {
            heap: allocator::heap(), wasm: bytes.clone(), prefix, suffix, unreached,
        });
    }
    measurements.receipt_ns = start.elapsed().as_nanos();
    measurements.proof = proofs.measurements;
    Ok((bytes, report, measurements))
}


fn check_wasm(source: &str, checked_source: std::sync::Arc<compiler_session::CheckedSource>,
    session: Option<&mut proof_session::Session>, executable: bool, measurements: &mut Measurements,
) -> Result<(std::sync::Arc<crate::ast::Program>, ir::Program, proof::Checked, proof_reach::Unreached), Diagnostic> {
    let logical = &checked_source.logical;
    let check_ns = measurements.primary_typed_check_ns;
    let start = Instant::now();
    let phase = start;
    let source_goals = if compiler_session::active() { checked_source.prepared()?.clone() } else { source_vc::prepare_with_sites(logical)? };
    measurements.vc_generation_ns = phase.elapsed().as_nanos();
    let phase = Instant::now();
    let nopanic: std::collections::BTreeMap<u32, std::ops::Range<usize>> = logical.functions.iter().filter_map(|f| Some((f.definition.0, f.nopanic.clone()?))).collect();
    // An executable proves what it can run and its own files (proof_reach.rs); a package, all.
    let catalog = executable.then(|| proof_reach::Catalog::of(source, &checked_source.syntax, &logical.functions));
    let (syntax,logical) = checked_source.into_parts();
    let mut checked = if executable { concrete::program(logical) } else { concrete::package_program(logical) }?;
    let unreached = catalog.map(|catalog| catalog.unreached(&checked.definitions.iter().copied().collect())).unwrap_or_default();
    measurements.concrete_planning_ns = phase.elapsed().as_nanos();
    measurements.check_ns = check_ns + start.elapsed().as_nanos();
    let start = Instant::now();
    let (source_goals, left_out) = unreached.select(source_goals);
    let proved = source_goals.proved_arithmetic.clone();
    let proofs = verify_build(&syntax, source, source_goals, session, &left_out)?;
    measurements.lean_invocations = proofs.invocations;
    measurements.proof_check_ns = start.elapsed().as_nanos();
    let start = Instant::now();
    check_nopanic(&mut checked, &proved, &nopanic)?;
    measurements.emit_ns = start.elapsed().as_nanos();
    Ok((syntax, checked, proofs, unreached))
}

fn check_package_bundle(source: &str, modules: &[usize], mut session: Option<&mut proof_session::Session>,
    gate: Gate,
) -> Result<(), Diagnostic> {
    let (checked, mut measurements) = analyze_gated(source, modules, session.as_deref_mut(), Some(gate), None, crate::parser::Target::Wasm)?;
    check_wasm(source, checked, session, false, &mut measurements).map(|_| ())
}

/// Check a package imported by a synthetic entry, including proofs and `nopanic` promises.
pub fn check_package(input: &std::path::Path) -> Result<(), String> {
    files::check_package(input)
}

fn check_nopanic(
    checked: &mut ir::Program,
    proved: &std::collections::BTreeSet<u32>,
    nopanic: &std::collections::BTreeMap<u32, std::ops::Range<usize>>,
) -> Result<(), Diagnostic> {
    // The proofs were accepted: what they show in range is emitted without a check.
    checked.proved_arithmetic = checked.definitions.iter().map(|d| proved.contains(d)).collect();
    checked.proved_index = checked.definitions.iter().map(|d| proved.contains(&(d | scalar_vc::INDEX_PROVED))).collect();
    checked.proved_slices = checked.definitions.iter().map(|d| proved.contains(&(d | scalar_vc::SLICE_PROVED))).collect();
    checked.proved_unreachable = checked.definitions.iter().map(|d| proved.contains(&(d | scalar_vc::UNREACHABLE_PROVED))).collect();
    let marked: Vec<bool> = checked.definitions.iter().map(|d| nopanic.contains_key(d)).collect();
    traps::nopanic(checked, &marked).map_err(|(f, why)| {
        let at = checked.definitions.get(f).and_then(|d| nopanic.get(d)).cloned().unwrap_or(0..0);
        Diagnostic::new("W0401", at, why)
    })
}

pub fn build_native(input: &std::path::Path, output: &std::path::Path, target: &str, measure: bool, manifest: Option<&std::path::Path>) -> Result<(), String> {
    build_native_with_manifest_digest(input, output, target, measure, manifest, None)
}

pub fn build_native_with_manifest_digest(input: &std::path::Path, output: &std::path::Path, target: &str, measure: bool,
    manifest: Option<&std::path::Path>, expected_manifest: Option<&str>,
) -> Result<(), String> {
    #[cfg(all(target_arch = "aarch64", any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
    if target == native::TARGET {
        return files::build_native(input, output, measure, manifest, expected_manifest);
    }
    let _ = (input, output, measure, manifest, expected_manifest);
    Err(format!("W0401: {target} requires witchyc built for that host"))
}

/// `source_goals` are those `proof_reach` selected; `left_out` is its account of the rest, for the report.
fn verify_build(syntax: &crate::ast::Program, source: &str, source_goals: proof_body::Prepared,
    session: Option<&mut proof_session::Session>, left_out: &str) -> Result<proof::Checked, Diagnostic> {
    let mut refusal = proof::Refusal::default();
    // A refused build's explanation probes take helpers started beside the refused one.
    let _spares = proof_session::Spares::open();
    match proof::check_prepared(syntax, source, source_goals, session, &mut refusal) {
        Ok(mut proofs) => { proofs.fresh.push_str(left_out); Ok(proofs) }
        Err(mut error) => {
            // Placed at the first function whose proof failed, which the explanation names.
            if let Some((at, message)) = proof::explain(syntax, source, refusal) && error.span == (0..0) {
                error.span = at;
                error.message = message;
            }
            Err(error)
        }
    }
}

/// Inspect actual checked operations. This private debug representation is not
/// an interchange format, bytecode input, or proof-admission evidence.
pub fn inspect(source: &str) -> Result<String, Diagnostic> { inspect_modules(source, &[]) }
fn inspect_modules(source: &str, modules: &[usize]) -> Result<String, Diagnostic> {
    if source.len() > MAX_PROGRAM_BYTES {
        return Err(Diagnostic::new(
            "W0400",
            0..source.len(),
            "source exceeds 1 MiB",
        ));
    }
    let syntax = parse_modules(source, modules)?;
    let logical = check::program(&syntax)?;
    let source_goals = source_vc::prepare(&logical)?;
    let checked = concrete::program(logical)?;
    proof::check(&syntax, source, source_goals)?;
    Ok(format!("{checked:#?}"))
}

/// Compile a bounded source file, then atomically replace the output module.
/// Refusal never changes the old output. This does not publish proof admission.
pub fn build(input: &std::path::Path, output: &std::path::Path) -> Result<(), String> {
    files::build(input, output, None).map(|_| ())
}

/// Atomically publish a module and return its scalar checker coverage receipt.
pub fn build_report(input: &std::path::Path, output: &std::path::Path) -> Result<String, String> {
    files::build(input, output, None).map(|(report, _)| report)
}

pub fn build_with_reviewed_input_key(input: &std::path::Path, output: &std::path::Path, key: &str) -> Result<(), String> {
    files::build(input, output, Some(key)).map(|_| ())
}

pub fn build_measured(
    input: &std::path::Path,
    output: &std::path::Path,
) -> Result<Measurements, String> {
    files::build(input, output, None).map(|(_, measurements)| measurements)
}

/// Each reachable function's capabilities, from the checked source (src/compiler/caps.rs).
/// `wasm-caps --by-file`: the same, per file of the program (caps.rs `by_file`).
pub fn caps_by_file(input: &std::path::Path) -> Result<String, String> {
    let (source, parts) = files::read_bundle(input)?;
    let (logical, main_parameters) = checked_bundle(&source, &parts)?;
    Ok(caps::by_file(&logical, main_parameters, &files::names(&parts, &here())))
}
/// `wasm-caps --policy POLICY PROGRAM`: the violations of a per-file authority policy (caps.rs
/// `check_policy`); an empty list means the program fits it.
pub fn caps_policy(policy: &std::path::Path, input: &std::path::Path) -> Result<Vec<String>, String> {
    let (source, parts) = files::read_bundle(input)?;
    caps_policy_of(policy, &source, &parts)
}
/// `caps_policy` for a bundle already read.
fn caps_policy_of(policy: &std::path::Path, source: &str, parts: &[files::Part]) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(policy).map_err(|e| format!("{}: {e}", policy.display()))?;
    let (logical, main_parameters) = checked_bundle(source, parts)?;
    // A policy names files from where it is kept, wherever the check runs from.
    let base = beside(policy);
    let mut violations = caps::check_policy(&caps::file_uses(&logical, main_parameters, &files::names(&parts, &base)), &text)?;
    violations.extend(caps::check_digests(&files::digests(parts, &base)?, &text)?);
    Ok(violations)
}
/// The directory a file is in (`.` for a bare name).
fn beside(path: &std::path::Path) -> std::path::PathBuf {
    match path.parent() { Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(), _ => std::path::PathBuf::from(".") }
}
fn here() -> std::path::PathBuf { std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")) }
/// `wasm-caps --repin POLICY PROGRAM`: each `; digest` of the policy set to the code its line
/// covers now, written back; the lines that changed.
pub fn caps_repin(policy: &std::path::Path, input: &std::path::Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(policy).map_err(|e| format!("{}: {e}", policy.display()))?;
    let (_, parts) = files::read_bundle(input)?;
    let (pinned, changed) = caps::repin(&files::digests(&parts, &beside(policy))?, &files::legacy_digests(&parts, &beside(policy))?, &text)?;
    if !changed.is_empty() { std::fs::write(policy, pinned).map_err(|e| format!("{}: {e}", policy.display()))?; }
    Ok(changed)
}
/// `wasm-caps --draft-policy PROGRAM`: a policy the program fits now (caps.rs `draft_policy`).
pub fn caps_draft_policy(input: &std::path::Path) -> Result<String, String> {
    let (source, parts) = files::read_bundle(input)?;
    let (logical, main_parameters) = checked_bundle(&source, &parts)?;
    // Drafted to be kept beside the program, as `PROGRAM.policy`, which every build then applies.
    let base = beside(input);
    caps::draft_policy(&caps::file_uses(&logical, main_parameters, &files::names(&parts, &base)), &files::digests(&parts, &base)?)
}
pub fn caps_file(input: &std::path::Path) -> Result<String, String> {
    let (logical, main_parameters) = checked_for_caps(input)?;
    Ok(caps::report(&logical, main_parameters))
}

/// `wasm-grants`: the engine's flags for a grant document checked against `main`
/// (grant_document.rs); with the review and warnings, or why the document does not fit.
pub fn grants_file(input: &std::path::Path, document: &std::path::Path) -> Result<(Vec<String>, Vec<String>, Vec<String>), Vec<String>> {
    grants_file_with_pin(input, document, None, None)
}

#[derive(PartialEq, Eq)]
struct GrantDocumentInput {
    text: String,
    base: std::path::PathBuf,
}

const MAX_GRANT_DOCUMENT_BYTES: usize = 32 << 20;

fn read_grant_document(document: &std::path::Path) -> Result<GrantDocumentInput, String> {
    let failure = |error| format!("{}: {error}", document.display());
    let resolved = std::fs::canonicalize(document).map_err(failure)?;
    let bytes = read_control_bytes(&resolved, MAX_GRANT_DOCUMENT_BYTES, "a grant document", true)?;
    let text = String::from_utf8(bytes).map_err(|error| format!("{}: {error}", document.display()))?;
    Ok(GrantDocumentInput { text, base: beside(&resolved) })
}

/// Check the same bytes and relative-path base the launcher captured.
pub fn grants_file_with_pin(input: &std::path::Path, document: &std::path::Path, pin: Option<&str>,
    expected_base: Option<&std::path::Path>,
) -> Result<(Vec<String>, Vec<String>, Vec<String>), Vec<String>> {
    grants_file_review(input, document, pin, expected_base, false).map(|(flags, review, warnings, _)| (flags, review, warnings))
}

pub fn grants_file_review(input: &std::path::Path, document: &std::path::Path, pin: Option<&str>,
    expected_base: Option<&std::path::Path>, include_key: bool,
) -> Result<(Vec<String>, Vec<String>, Vec<String>, Option<String>), Vec<String>> {
    let (source, parts) = files::read_bundle(input).map_err(|e| vec![e])?;
    let (checked, _) = compiler_session::analyze(&source, &files::starts(&parts))
        .map_err(|e| vec![files::render(&source, &parts, &e)])?;
    let grant = read_grant_document(document).map_err(|e| vec![e])?;
    if let Some(base) = expected_base && base != grant.base {
        return Err(vec![format!("{}: grant document directory changed before review", document.display())]);
    }
    if let Some(expected) = pin {
        if !proof_digest::is_pin(expected) {
            return Err(vec!["invalid grant document SHA-384 pin".into()]);
        }
        let actual = proof_digest::pin(grant.text.as_bytes()).map_err(|e| vec![e.to_string()])?;
        if actual != expected {
            return Err(vec![format!("{}: grant document changed before review", document.display())]);
        }
    }
    let (flags, review, warnings) = grants_checked(&checked.syntax, document, &grant.text, &grant.base)?;
    let key = if include_key {
        let policy = compiler_session::optional_text(&input.with_extension("policy")).map_err(|e| vec![e])?;
        let binding = compiler_session::binding().map_err(|e| vec![e])?;
        Some(files::input_key(&parts, policy.as_deref(), binding.as_deref()).map_err(|e| vec![e])?)
    } else { None };
    Ok((flags, review, warnings, key))
}
fn grants_checked(syntax: &crate::ast::Program, document: &std::path::Path, text: &str, base: &std::path::Path) -> Result<(Vec<String>, Vec<String>, Vec<String>), Vec<String>> {
    let parsed = grant_document::parse(text).map_err(|e| vec![format!("{}: {e}", document.display())])?;
    let parameters = grant_parameters(syntax)?;
    let granted = grant_document::check(&parsed, &parameters, base)?;
    Ok((granted.flags, granted.review, granted.warnings))
}

/// A launch manifest checked against the checked program's `main`: the plan a native
/// executable embeds (grant_document/launch.rs). `digest` is the manifest's SHA-256.
pub(super) fn launch_plan_checked(syntax: &crate::ast::Program, text: &str, digest: &str) -> Result<grant_document::LaunchPlan, Vec<String>> {
    let parsed = grant_document::parse(text).map_err(|e| vec![format!("launch manifest: {e}")])?;
    grant_document::launch_plan(&parsed, &grant_parameters(syntax)?, digest)
}

/// The manifest's UTF-8 text and its digest, from one bounded regular-file read.
pub(super) fn read_manifest(path: &std::path::Path) -> Result<(String, String), String> {
    let text = String::from_utf8(read_manifest_bytes(path)?)
        .map_err(|_| format!("{}: a launch manifest is UTF-8", path.display()))?;
    let digest = proof_digest::bytes(text.as_bytes()).map_err(|e| e.to_string())?;
    Ok((text, digest))
}

fn read_manifest_bytes(path: &std::path::Path) -> Result<Vec<u8>, String> {
    read_control_bytes(path, grant_document::MAX_MANIFEST_BYTES, "a launch manifest", false)
}

fn read_control_bytes(path: &std::path::Path, limit: usize, kind: &str, nofollow: bool) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let report = |error| format!("{}: {error}", path.display());
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Inspect the object only after opening it, without waiting for a writer.
        let flags = libc::O_NONBLOCK | if nofollow { libc::O_NOFOLLOW } else { 0 };
        options.custom_flags(flags);
    }
    #[cfg(not(unix))]
    let _ = nofollow;
    let file = options.open(path).map_err(report)?;
    let before = file.metadata().map_err(report)?;
    if !before.is_file() {
        return Err(format!("{}: {kind} is a regular file", path.display()));
    }
    if before.len() > limit as u64 {
        return Err(format!("{}: {kind} is at most {} KiB", path.display(), limit / 1024));
    }
    let mut bytes = Vec::new();
    (&file).take(before.len() + 1).read_to_end(&mut bytes).map_err(report)?;
    let after = file.metadata().map_err(report)?;
    if bytes.len() as u64 != before.len() || before.modified().ok() != after.modified().ok() {
        return Err(format!("{}: {kind} changed while being read", path.display()));
    }
    Ok(bytes)
}

/// `witchyc native-plan FILE MANIFEST`: the launch plan a manifest-bound native executable of
/// FILE would embed, as review lines (sources and limits, never a resolved value).
pub fn native_plan_file(input: &std::path::Path, manifest: &std::path::Path) -> Result<Vec<String>, Vec<String>> {
    native_plan_file_with_digest(input, manifest, None)
}

pub fn native_plan_file_with_digest(input: &std::path::Path, manifest: &std::path::Path, expected_manifest: Option<&str>) -> Result<Vec<String>, Vec<String>> {
    let (source, parts) = files::read_bundle(input).map_err(|e| vec![e])?;
    let (checked, _) = compiler_session::analyze(&source, &files::starts(&parts)).map_err(|e| vec![files::render(&source, &parts, &e)])?;
    let (text, digest) = read_manifest(manifest).map_err(|e| vec![e])?;
    if let Some(expected) = expected_manifest && digest != expected {
        return Err(vec!["launch manifest changed before review".into()]);
    }
    Ok(launch_plan_checked(&checked.syntax, &text, &digest)?.review)
}

/// `main`'s grantable parameters, by name and rendered type, with the grantable capabilities
/// built at its top.
fn grant_parameters(syntax: &crate::ast::Program) -> Result<Vec<grant_document::Parameter>, Vec<String>> {
    use crate::ast::{Declaration, Expression, Statement};
    let main = syntax.declarations.iter().find_map(|d| match d { Declaration::Function(f) if f.name == "main" => Some(f), _ => None }).ok_or_else(|| vec!["the program has no main".to_string()])?;
    let mut parameters: Vec<grant_document::Parameter> = main.parameters.iter().filter(|(n, _)| n != "#grants")
        .map(|(name, ty)| grant_document::Parameter { name: name.clone(), ty: ty.render(), grantable: false }).collect();
    // The grantable capabilities, built at main's top (parser/grants.rs).
    for statement in &main.body {
        let Statement::Bind { name, value, .. } = statement else { break };
        let Expression::Call(callee, _) = &value.kind else { break };
        let Expression::Name(called) = &callee.kind else { break };
        let Some(ty) = called.strip_prefix("#grant_") else { break };
        parameters.push(grant_document::Parameter { name: name.clone(), ty: ty.split('@').next().unwrap_or(ty).to_string(), grantable: true });
    }
    Ok(parameters)
}

/// How each reachable function can stop the run with a trap (src/compiler/traps.rs), from the
/// program as it is emitted; no proof is needed to read it.
pub fn traps_file(input: &std::path::Path) -> Result<String, String> {
    let (source, parts) = files::read_bundle(input)?;
    let render = |e: Diagnostic| files::render(&source, &parts, &e);
    let syntax = parse_modules(&source, &files::starts(&parts)).map_err(render)?;
    let logical = check::program(&syntax).map_err(render)?;
    // As `wasm-checked` would emit it, which it does only once these goals are proved.
    let proved = source_vc::prepare_with_sites(&logical).map_err(render)?.proved_arithmetic;
    let mut program = concrete::program(logical).map_err(render)?;
    program.proved_arithmetic = program.definitions.iter().map(|d| proved.contains(d)).collect();
    program.proved_index = program.definitions.iter().map(|d| proved.contains(&(d | scalar_vc::INDEX_PROVED))).collect();
    program.proved_slices = program.definitions.iter().map(|d| proved.contains(&(d | scalar_vc::SLICE_PROVED))).collect();
    for name in &mut program.names {
        if let Some(rest) = name.strip_prefix("Closure") && let Some((offset, method)) = rest.split_once('.')
            && let Ok(at) = offset.parse::<usize>() && let Some(line) = files::line_of(&source, &parts, at) {
            *name = format!("a closure at {line}.{method}");
        }
    }
    Ok(traps::report(&program))
}

/// `caps_file` as JSON, for tools.
pub fn caps_json_file(input: &std::path::Path) -> Result<String, String> {
    let (logical, main_parameters) = checked_for_caps(input)?;
    Ok(caps::json(&logical, main_parameters))
}

/// How the authority of `new` differs from `old`, and whether it widened.
pub fn caps_diff_files(old: &std::path::Path, new: &std::path::Path) -> Result<(String, bool), String> {
    let (before, before_main) = checked_for_caps(old)?;
    let (after, after_main) = checked_for_caps(new)?;
    Ok(caps::diff(&caps::footprint(&before, before_main), &caps::footprint(&after, after_main)))
}

fn checked_for_caps(input: &std::path::Path) -> Result<(logical_body::Program, usize), String> {
    let (source, parts) = files::read_bundle(input)?;
    checked_bundle(&source, &parts)
}
/// `checked_for_caps` of a bundle already read: a build checks its policy against the very
/// text it compiles (automated review: reading the files again could judge other bytes).
fn checked_bundle(source: &str, parts: &[files::Part]) -> Result<(logical_body::Program, usize), String> {
    let render = |e: Diagnostic| files::render(&source, &parts, &e);
    // With each file's bounds, as a build parses it: privacy and qualified names depend on them.
    let (checked, _) = compiler_session::analyze(source, &files::starts(parts)).map_err(render)?;
    let (syntax,mut logical) = checked.into_parts();
    let main_parameters = caps::main_grant(&syntax);
    // A closure is reported by where it is written (`a closure (at main.wy:12:9)`), not by the
    // byte offset its record is named by.
    // Its record is renamed the same way, so a method reached through it (`WitchyOnce0.call_once`
    // given that record) is found by name (review: closures run by `race` were missing).
    let mut records = Vec::new();
    for f in &mut logical.functions {
        if let Some(rest) = f.name.strip_prefix("Closure#") && let Some((offset, method)) = rest.split_once('.')
            && let Ok(at) = offset.parse::<usize>() && let Some(line) = files::line_of(&source, &parts, at) {
            // With its column: two closures on one line are two records.
            let column = source.get(..at).map_or(0, |before| at - before.rfind('\n').map_or(0, |newline| newline + 1)) + 1;
            let line = format!("{line}:{column}");
            records.push((format!("Closure#{offset}"), format!("Closure#{line}")));
            f.name = format!("Closure#{line}.{method}");
        }
    }
    for (old, new) in records { logical.types.rename_record(&old, &new); }
    Ok((logical, main_parameters))
}

pub fn inspect_file(input: &std::path::Path) -> Result<String, String> {
    let (source, parts) = files::read_bundle(input)?;
    inspect_modules(&source, &files::starts(&parts)).map_err(|e| files::render(&source, &parts, &e))
}

#[cfg(test)]
mod record_tests;
#[cfg(test)]
mod record_post_tests;
#[cfg(test)]
mod record_take_tests;

/// Explicit nonpublishing experiment. The default compiler never constructs this backend.
/// Bounded input lines: check REQUEST_ID EXPECTED_SOURCE_SHA256. First refusal stops the session.
pub fn diagnostic_loop(path: &std::path::Path) -> Result<(), String> {
    diagnostic_session_loop(path, proof_session::Session::default())
}
/// Explicit owner-disciplined private-generation experiment; never portable publication.
pub fn diagnostic_trusted_generation_loop(path: &std::path::Path, expected: &str) -> Result<(), String> {
    let session = proof_session::Session::trusted_generation(expected).map_err(|e| format!("{}: {}", e.code, e.message))?;
    diagnostic_session_loop(path, session)
}
/// Isolated nonpublishing body-interface reuse; current caller and unused proofs remain fresh.
pub fn diagnostic_body_interface_loop(path: &std::path::Path, expected: &str) -> Result<(), String> {
    let session = proof_session::Session::body_generation(expected).map_err(|e| format!("{}: {}", e.code, e.message))?;
    diagnostic_session_loop(path, session)
}
fn diagnostic_session_loop(path: &std::path::Path, mut session: proof_session::Session) -> Result<(), String> {
    use std::io::{BufRead, Read, Write};
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    loop {
        let mut line = String::new();
        let count = input.by_ref().take(257).read_line(&mut line).map_err(|_| "diagnostic input I/O")?;
        if count == 0 { return session.finish().map_err(|e| format!("{}: {}", e.code, e.message)); }
        if count > 256 || !line.ends_with('\n') { return Err("diagnostic request line exceeds bound or is truncated".into()); }
        let parts = line.split_whitespace().collect::<Vec<_>>();
        let ["check", id, expected] = parts.as_slice() else { return Err("expected check REQUEST_ID SOURCE_SHA256".into()); };
        if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err("diagnostic request identity refused".into());
        }
        let read_start = Instant::now();
        let (source, parts) = files::read_bundle(path)?;
        let read_ns = read_start.elapsed().as_nanos();
        let source_id = proof_digest::bytes(source.as_bytes()).map_err(|_| "diagnostic source identity I/O")?;
        if source_id != *expected { return Err("diagnostic source differs from expected identity".into()); }
        session.request(id, &source_id);
        let (wasm, report, mut measurements) = compile_backend(&source, &files::starts(&parts), Some(&mut session))
            .map_err(|e| format!("{} {:?}: {}", e.code, e.span, e.message))?;
        measurements.read_ns = read_ns;
        let after = files::read_source(path)?;
        if proof_digest::bytes(after.as_bytes()).map_err(|_| "diagnostic source recheck I/O")? != source_id {
            return Err("diagnostic source changed during check".into());
        }
        let wasm_id = proof_digest::bytes(&wasm).map_err(|_| "diagnostic Wasm identity I/O")?;
        session.validate_idle().map_err(|e| format!("{}: {}", e.code, e.message))?;
        let mode = session.mode();
        let trust = session.trust_fields();
        writeln!(output, "{{\"schema\":\"witchy.compiler.session.diagnostic.v1\",\"mode\":\"{mode}\"{trust},\"request_id\":\"{id}\",\"source_sha256\":\"{source_id}\",\"wasm_sha256\":\"{wasm_id}\",\"native_helper_used\":{},\"check\":{report},\"measurements\":{}}}", measurements.lean_invocations != 0, measurements.json())
            .map_err(|_| "diagnostic output I/O")?;
        output.flush().map_err(|_| "diagnostic output flush I/O")?;
    }
}

/// Read-only content identity; this grants no proof or immutability authority.
pub fn toolchain_identity(root: &std::path::Path) -> Result<String, String> {
    if !root.is_absolute() { return Err("toolchain identity requires an absolute SDK root".into()); }
    let root = root.canonicalize().map_err(|_| "toolchain root identity I/O refused")?;
    let (domain, identity) = proof_digest::installation_identity(&root).map_err(|_| "toolchain content identity I/O refused")?;
    Ok(format!("{{\"algorithm\":\"sha256\",\"domain\":\"{domain}\",\"hex\":\"{identity}\"}}"))
}

#[cfg(test)]
mod record_ownership_tests;
