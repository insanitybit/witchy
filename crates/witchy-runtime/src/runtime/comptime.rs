//! Explicit canonical compiler-session launch. Ordinary runtime launchers do
//! not install its imports. No source evaluator or ambient grants are used.
use super::*;
use witchy_syntax::compiler_session::{CompilerSyntaxSession, DeterministicBudget};
use witchy_syntax::compiler_session::{seeds::{CompilerSyntaxSeeds, SyntaxCost, EmittedExpression, EmittedItem}, ComptimeSyntaxOrigin};

#[derive(Debug, Clone, Copy)]
pub struct ComptimeLimits {
    pub fuel: u64,
    pub memory_pages: usize,
    pub fresh_identifiers: u64,
    pub host_output_bytes: u64,
    pub syntax_nodes: u64,
    pub syntax_bytes: u64,
    pub emitted_nodes: u64,
    pub emitted_bytes: u64,
}

pub struct ComptimeOutcome {
    pub result: i64,
    pub fresh_identifiers: Vec<String>,
    pub fuel_consumed: u64,
    pub heap_leaks: usize,
    pub expressions: Vec<EmittedExpression>,
    pub items: Vec<EmittedItem>,
    source_digest: [u8; 32],
}

impl ComptimeOutcome {
    /// Identity of the admitted source recipe, never supplied by the guest.
    /// The compiler can compare this with the expected checked predecessor
    /// before consuming any generated syntax.
    pub fn source_digest(&self) -> [u8; 32] { self.source_digest }
}

pub(super) struct SessionState {
    pub syntax: CompilerSyntaxSession,
    pub names: DeterministicBudget,
    pub bytes: DeterministicBudget,
    pub input_bytes: u64,
    pub issued: Vec<String>,
    pub seeds: std::sync::Arc<CompilerSyntaxSeeds>,
    pub costs: std::collections::BTreeMap<String, SyntaxCost>,
    pub item_costs: std::collections::BTreeMap<String, SyntaxCost>,
    pub type_costs: std::collections::BTreeMap<String, SyntaxCost>,
    pub param_costs: std::collections::BTreeMap<String, SyntaxCost>,
    pub item_origins: std::collections::BTreeMap<String, ComptimeSyntaxOrigin>,
    pub nodes: DeterministicBudget,
    pub syntax_bytes: DeterministicBudget,
    pub emitted_nodes: DeterministicBudget,
    pub emitted_bytes: DeterministicBudget,
    pub expressions: Vec<EmittedExpression>,
    pub items: Vec<EmittedItem>,
}

impl Runtime {
    /// Execute the exact admitted zero-grant compiler recipe. This initial
    /// operation surface supports fresh names and typed expression quotation
    /// and emission, not arbitrary syntax builders or source-text evaluation.
    /// Wasm instructions and host byte work consume deterministic budgets.
    pub fn run_comptime(
        artifact: &AdmittedCanonicalArtifact, limits: ComptimeLimits,
    ) -> Result<ComptimeOutcome> {
        let contract = artifact.compiler_session()
            .ok_or_else(|| Error::msg("comptime launch requires an admitted compiler recipe"))?;
        let entry = artifact.entry().ok_or_else(|| Error::msg("comptime recipe lacks entry"))?;
        if !entry.capabilities().is_empty() || entry.result() != CanonicalEntryResult::Int {
            bail!("comptime recipe entry must be parameterless Int");
        }
        if limits.memory_pages.checked_mul(65536).is_none() {
            bail!("comptime memory limit overflow");
        }
        let mut runtime = Runtime::configured_with_fuel(false, wasmtime::OptLevel::None, false, true)?;
        let module = WorkerModule::from_admitted(&runtime.engine, artifact)?;
        let mut syntax = CompilerSyntaxSession::default();
        syntax.set_invocation(Some(contract.scope().into()), None, None);
        let session = SessionState {
            syntax, names: DeterministicBudget::new(limits.fresh_identifiers),
            bytes: DeterministicBudget::new(limits.host_output_bytes), issued: Vec::new(),
            input_bytes: limits.host_output_bytes,
            seeds: artifact.compiler_syntax_seeds().cloned()
                .ok_or_else(|| Error::msg("comptime artifact lacks checked syntax seeds"))?,
            costs: Default::default(), item_costs: Default::default(), type_costs: Default::default(),
            param_costs: Default::default(), item_origins: Default::default(),
            nodes: DeterministicBudget::new(limits.syntax_nodes),
            syntax_bytes: DeterministicBudget::new(limits.syntax_bytes),
            emitted_nodes: DeterministicBudget::new(limits.emitted_nodes),
            emitted_bytes: DeterministicBudget::new(limits.emitted_bytes), expressions: Vec::new(), items: Vec::new(),
        };
        let mut vm = runtime.instantiate_module_with_session(
            module, Capabilities::none(), limits.memory_pages, Some(entry.clone()), &[],
            Some((session, limits.fuel)),
        )?;
        let call = vm.instance.get_typed_func::<(), i64>(&mut vm.store, entry.export())?;
        let result = call.call(&mut vm.store, ())?;
        vm.heap_sweep()?;
        let heap_leaks = vm.canonical_heap_leaks()?.ok_or_else(|| Error::msg("comptime artifact lacks heap audit"))?;
        if heap_leaks != 0 { bail!("comptime recipe leaked {heap_leaks} canonical roots"); }
        let fuel_consumed = limits.fuel - vm.store.get_fuel()?;
        let session = vm.store.data_mut().compiler_syntax.take().expect("comptime session installed");
        Ok(ComptimeOutcome { result, fresh_identifiers: session.issued, fuel_consumed, heap_leaks,
            expressions: session.expressions, items: session.items,
            source_digest: contract.source_digest() })
    }
}
