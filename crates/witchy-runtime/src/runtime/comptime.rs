//! Explicit canonical compiler-session launch. Ordinary runtime launchers do
//! not install its imports. No source evaluator or ambient grants are used.
use super::*;
use witchy_syntax::compiler_session::{CompilerSyntaxSession, DeterministicBudget};

#[derive(Debug, Clone, Copy)]
pub struct ComptimeLimits {
    pub fuel: u64,
    pub memory_pages: usize,
    pub fresh_identifiers: u64,
    pub host_output_bytes: u64,
}

pub struct ComptimeOutcome {
    pub result: i64,
    pub fresh_identifiers: Vec<String>,
    pub fuel_consumed: u64,
    pub heap_leaks: usize,
}

pub(super) struct SessionState {
    pub syntax: CompilerSyntaxSession,
    pub names: DeterministicBudget,
    pub bytes: DeterministicBudget,
    pub issued: Vec<String>,
}

impl Runtime {
    /// Execute the exact admitted zero-grant compiler recipe. This initial
    /// operation surface supports fresh names, not arbitrary syntax emission.
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
        Ok(ComptimeOutcome { result, fresh_identifiers: session.issued, fuel_consumed, heap_leaks })
    }
}
